"""Unit tests for Mojang vanilla 1.7.10 bootstrap; never download game files."""
import hashlib
import io
import json
import zipfile
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
import vanilla1710 as target


class Vanilla1710LauncherTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name) / "server"

    def test_exact_official_server_identity_is_pinned(self):
        self.assertEqual(target.VERSION, "1.7.10")
        self.assertEqual(target.SERVER_SHA1, "952438ac4e01b4d115c5fc38f891710c4941df29")
        self.assertEqual(target.SERVER_SIZE, 9605030)
        for url in target.MOJANG_URLS:
            self.assertTrue(url.startswith("https://"))
            self.assertIn(f"/{target.SERVER_SHA1}/server.jar", url)
            self.assertTrue(
                url.split("/")[2] in ("launcher.mojang.com", "piston-data.mojang.com")
            )

    def test_prepare_is_idempotent_and_secure(self):
        with mock.patch.object(target, "download_server") as download, \
             mock.patch.object(target, "download_log4j") as download_xml:
            target.prepare(self.root, "127.0.0.1", 25565, 8)
            target.prepare(self.root, "0.0.0.0", 25566, 20)
        self.assertEqual(download.call_count, 2)
        self.assertEqual(download_xml.call_count, 2)
        props = target.parse_properties(self.root / "server.properties")
        self.assertEqual(props["online-mode"], "true")
        self.assertEqual(props["white-list"], "true")
        self.assertEqual(props["server-ip"], "127.0.0.1")
        self.assertEqual(props["server-port"], "25565")
        self.assertEqual(props["gamemode"], "0")
        self.assertEqual(props["spawn-monsters"], "true")
        self.assertEqual(props["enable-rcon"], "false")
        self.assertEqual((self.root / "whitelist.json").read_text(), "[]\n")
        self.assertIn("eula=false", (self.root / "eula.txt").read_text())
        target.check_security(self.root, require_eula=False)
        with self.assertRaises(target.SetupError):
            target.check_security(self.root, require_eula=True)

    def test_invalid_configuration_is_fail_closed(self):
        with mock.patch.object(target, "download_server"), mock.patch.object(target, "download_log4j"):
            target.prepare(self.root, "127.0.0.1", 25565, 8)
        original = (self.root / "server.properties").read_text()
        for changed in (
            original.replace("online-mode=true", "online-mode=false"),
            original.replace("white-list=true", "white-list=false"),
            original.replace("enable-rcon=false", "enable-rcon=true"),
            original.replace("server-port=25565", "server-port=99999"),
            original.replace("server-ip=127.0.0.1", "server-ip=invalid"),
        ):
            (self.root / "server.properties").write_text(changed)
            with self.assertRaises(target.SetupError):
                target.check_security(self.root, require_eula=False)
        (self.root / "server.properties").write_text(original)
        (self.root / "whitelist.json").write_text('{"not":"a list"}')
        with self.assertRaises(target.SetupError):
            target.check_security(self.root, require_eula=False)

    def test_requires_explicit_eula_consent(self):
        with mock.patch.object(target, "download_server"), mock.patch.object(target, "download_log4j"):
            target.prepare(self.root, "127.0.0.1", 25565, 8)
        eula = self.root / "eula.txt"
        eula.write_text("eula=false\n")
        with self.assertRaises(target.SetupError):
            target.check_security(self.root, require_eula=True)
        eula.write_text("eula=true\n")
        target.check_security(self.root, require_eula=True)

    def test_preexisting_jar_never_overwritten(self):
        self.root.mkdir()
        jar = self.root / target.JAR_NAME
        jar.write_bytes(b"untrusted data")
        with self.assertRaises(target.SetupError):
            target.download_server(jar)
        self.assertEqual(jar.read_bytes(), b"untrusted data")

    def test_download_checks_both_hash_and_size_without_network(self):
        content = b"known test jar content"
        digest = hashlib.sha1(content).hexdigest()
        self.root.mkdir()
        jar = self.root / target.JAR_NAME
        with mock.patch.object(target, "SERVER_SHA1", digest), \
             mock.patch.object(target, "SERVER_SIZE", len(content)), \
             mock.patch.object(target, "MOJANG_URLS", ("https://launcher.mojang.com/test",)), \
             mock.patch.object(target, "urlopen", return_value=io.BytesIO(content)):
            target.download_server(jar)
        self.assertEqual(jar.read_bytes(), content)
        target.verify_jar(jar, sha1=digest, size=len(content))
        jar.write_bytes(b"tampered")
        with self.assertRaises(target.SetupError):
            target.verify_jar(jar, sha1=digest, size=len(content))

    def test_wrong_network_response_does_not_leave_jar(self):
        self.root.mkdir()
        jar = self.root / target.JAR_NAME
        with mock.patch.object(target, "MOJANG_URLS", ("https://launcher.mojang.com/test",)), \
             mock.patch.object(target, "urlopen", return_value=io.BytesIO(b"incorrect")):
            with self.assertRaises(target.SetupError):
                target.download_server(jar)
        self.assertFalse(jar.exists())
        self.assertEqual(list(self.root.glob(".minecraft-1710-*.jar")), [])

    def test_required_log4j_patch_cannot_be_missing_or_tampered(self):
        self.root.mkdir()
        xml = self.root / target.LOG4J_NAME
        self.assertEqual(target.LOG4J_SHA1, "dd2b723346a8dcd48e7f4d245f6bf09e98db9696")
        with self.assertRaises(target.SetupError):
            target.verify_log4j(xml)
        content = b"<Configuration/>"
        with mock.patch.object(target, "LOG4J_SHA1", hashlib.sha1(content).hexdigest()), \
             mock.patch.object(target, "LOG4J_URLS", ("https://launcher.mojang.com/test",)), \
             mock.patch.object(target, "urlopen", return_value=io.BytesIO(content)):
            target.download_log4j(xml)
        self.assertEqual(xml.read_bytes(), content)
        xml.write_text("corrupted XML")
        with self.assertRaises(target.SetupError):
            target.verify_log4j(xml)

    def test_startup_includes_mojang_recommended_log4j_config(self):
        with mock.patch.object(target, "download_server"), mock.patch.object(target, "download_log4j"):
            target.prepare(self.root, "127.0.0.1", 25565, 8)
        (self.root / "eula.txt").write_text("eula=true\n")
        process = mock.Mock(returncode=0)
        with mock.patch.object(target, "verify_jar"), \
             mock.patch.object(target, "verify_log4j"), \
             mock.patch.object(target, "find_java", return_value="java"), \
             mock.patch.object(target, "check_java"), \
             mock.patch.object(target.subprocess, "run", return_value=process) as runner:
            self.assertEqual(target.run_server(self.root, None, "2G"), 0)
        args=runner.call_args.args[0]
        self.assertIn("-Dlog4j.configurationFile=log4j2_17-111.xml", args)
        self.assertIn("-Xmx2G", args)
        self.assertIn(target.JAR_NAME, args)

    def test_java8_check_rejects_modern_jvm(self):
        java8 = mock.Mock(returncode=0, stdout="", stderr='openjdk version "1.8.0_472"\n')
        java17 = mock.Mock(returncode=0, stdout="", stderr='openjdk version "17.0.15"\n')
        with mock.patch.object(target.subprocess, "run", return_value=java8):
            target.check_java("java")
        with mock.patch.object(target.subprocess, "run", return_value=java17):
            with self.assertRaises(target.SetupError):
                target.check_java("java")

    def test_run_never_starts_java_without_eula(self):
        with mock.patch.object(target, "download_server"), mock.patch.object(target, "download_log4j"):
            target.prepare(self.root, "127.0.0.1", 25565, 8)
        with mock.patch.object(target.subprocess, "run") as process:
            with self.assertRaises(target.SetupError):
                target.run_server(self.root, "java", "2G")
            process.assert_not_called()

    def test_status_detects_protocol5_and_rejects_wrong_version(self):
        class MockSocket:
            def __init__(self, payload):
                self.buffer = bytearray(payload)
                self.sent = b""
            def __enter__(self):
                return self
            def __exit__(self, *unused):
                return False
            def settimeout(self, unused):
                pass
            def sendall(self, data):
                self.sent += data
            def recv(self, length):
                if not self.buffer:
                    return b""
                size = min(3, length, len(self.buffer))
                chunk = bytes(self.buffer[:size])
                del self.buffer[:size]
                return chunk

        def mock_status(protocol):
            status = json.dumps({
                "version": {"name": "1.7.10", "protocol": protocol},
                "players": {"max": 8, "online": 2},
            }).encode("utf-8")
            body = b"\x00" + target.encode_varint(len(status)) + status
            return MockSocket(target.encode_varint(len(body)) + body)
        good = mock_status(5)
        with mock.patch.object(target.socket, "create_connection", return_value=good):
            result = target.query_status("127.0.0.1", 25565)
        self.assertEqual(result["players"]["online"], 2)
        self.assertIn(b"\x00\x05", good.sent)
        wrong = mock_status(47)
        with mock.patch.object(target.socket, "create_connection", return_value=wrong):
            with self.assertRaises(target.SetupError):
                target.query_status("127.0.0.1", 25565)

    def test_backup_works_offline_and_never_overwrites(self):
        with mock.patch.object(target, "download_server"), \
             mock.patch.object(target, "download_log4j"):
            target.prepare(self.root, "127.0.0.1", 25565, 8)
        world = self.root / "world"
        (world / "region").mkdir(parents=True)
        (world / "level.dat").write_bytes(b"test-level-file")
        (world / "region" / "r.0.0.mca").write_bytes(b"test-region-file")
        (self.root / "usercache.json").write_text("[]")
        output = self.root.parent / "backup.zip"
        with mock.patch.object(target, "is_port_listening", return_value=False):
            self.assertEqual(target.backup_server(self.root, output), 5)
            self.assertFalse((self.root / ".server-operation.lock").exists())
            with self.assertRaises(target.SetupError):
                target.backup_server(self.root, output)
        with zipfile.ZipFile(output, "r") as archive:
            self.assertIn("world/level.dat", archive.namelist())
            self.assertIn("world/region/r.0.0.mca", archive.namelist())
            self.assertIn("server.properties", archive.namelist())
            self.assertIn("whitelist.json", archive.namelist())
            self.assertIn("usercache.json", archive.namelist())
            self.assertNotIn(target.JAR_NAME, archive.namelist())
            self.assertNotIn(target.LOG4J_NAME, archive.namelist())
            self.assertEqual(archive.read("world/level.dat"), b"test-level-file")

    def test_backup_refuses_running_or_concurrent_server(self):
        with mock.patch.object(target, "download_server"), \
             mock.patch.object(target, "download_log4j"):
            target.prepare(self.root, "127.0.0.1", 25565, 8)
        world = self.root / "world"
        world.mkdir()
        (world / "level.dat").write_bytes(b"known data")
        output = self.root.parent / "backup.zip"
        with mock.patch.object(target, "is_port_listening", return_value=True):
            with self.assertRaises(target.SetupError):
                target.backup_server(self.root, output)
        self.assertFalse(output.exists())
        (self.root / ".server-operation.lock").write_text("another owner")
        with mock.patch.object(target, "is_port_listening", return_value=False):
            with self.assertRaises(target.SetupError):
                target.backup_server(self.root, output)
        self.assertFalse(output.exists())

    def test_backup_rejects_symlinks(self):
        with mock.patch.object(target, "download_server"), \
             mock.patch.object(target, "download_log4j"):
            target.prepare(self.root, "127.0.0.1", 25565, 8)
        world = self.root / "world"
        world.mkdir()
        (world / "level.dat").write_bytes(b"data")
        (world / "private.link").symlink_to(self.root / "server.properties")
        with mock.patch.object(target, "is_port_listening", return_value=False):
            with self.assertRaises(target.SetupError):
                target.backup_server(self.root, self.root.parent / "backup.zip")
        self.assertFalse((self.root.parent / "backup.zip").exists())

    def test_invalid_prepare_options(self):
        with self.assertRaises(target.SetupError):
            target.prepare(self.root, "127.0.0.1", 0, 8)
        with self.assertRaises(target.SetupError):
            target.prepare(self.root, "127.0.0.1", 25565, 0)


if __name__ == "__main__":
    unittest.main()

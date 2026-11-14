"""Unit tests for NEXT-193's external verifier; not native-server proof."""
from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock

from verify_next193_failed_boot_immutability import probe_failed_boot


class Next193VerifierTests(unittest.TestCase):
    def setUp(self) -> None:
        self.context = tempfile.TemporaryDirectory(prefix="ir-next193-tests-")
        root = Path(self.context.name)
        self.fixture = root / "fixture.irw"
        self.fixture.write_bytes(b"IRW4_ORIGINAL_WORLD_0123456789")
        self.server = root / "fake-server"
        self.server.write_bytes(b"fake-server-for-argument-validation")
        self.original = self.fixture.read_bytes()

    def tearDown(self) -> None:
        self.assertEqual(self.fixture.read_bytes(), self.original)
        self.context.cleanup()

    def test_failed_boot_without_mutation_passes(self) -> None:
        runner = Mock(return_value=subprocess.CompletedProcess([], 1, b"", b""))
        result = probe_failed_boot(self.server, self.fixture, runner=runner)
        self.assertEqual(result["result"], "PASS")
        self.assertEqual(len(result["files_sha256"]), 2)
        args, kwargs = runner.call_args
        cmd = args[0]
        self.assertNotIn(str(self.fixture), cmd)
        self.assertEqual(cmd[cmd.index("--bind") + 1], "127.0.0.1:0")
        self.assertEqual(kwargs["cwd"], Path(cmd[cmd.index("--world") + 1]).parent)

    def test_broken_boot_that_changes_world_is_detected(self) -> None:
        def mutating_runner(command: list[str], **_: object) -> subprocess.CompletedProcess[bytes]:
            Path(command[command.index("--world") + 1]).write_bytes(b"MUTATED")
            return subprocess.CompletedProcess(command, 1, b"", b"")

        with self.assertRaisesRegex(AssertionError, "modified sandbox world files"):
            probe_failed_boot(self.server, self.fixture, runner=mutating_runner)

    def test_successful_boot_must_not_pass(self) -> None:
        with self.assertRaisesRegex(AssertionError, "accepted corrupted world"):
            probe_failed_boot(
                self.server, self.fixture,
                runner=lambda command, **kw: subprocess.CompletedProcess(command, 0, b"", b""),
            )

    def test_fixture_validation_rejects_symlink(self) -> None:
        link = self.fixture.parent / "linked.irw"
        try:
            link.symlink_to(self.fixture)
        except OSError:
            self.skipTest("symlinks unavailable")
        with self.assertRaisesRegex(ValueError, "not a symlink"):
            probe_failed_boot(self.server, link)

    def test_timeout_range_is_bounded(self) -> None:
        with self.assertRaisesRegex(ValueError, "timeout"):
            probe_failed_boot(self.server, self.fixture, timeout=10000)


if __name__ == "__main__":
    unittest.main()

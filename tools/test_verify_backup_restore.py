"""Restore drills never mutate the original, trust an unmarked tree, or claim GUI acceptance."""
import importlib.util
import io
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "verify_backup_restore", Path(__file__).with_name("verify_backup_restore.py")
)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


# Each invocation starts with an uncached tempfile module (or an explicit
# cached directory), so the standard library's own candidate probe is tested.
# Block every write attempt inside the synthetic backup and stop before any
# scratch allocation/copy can recurse into that backup.
_UNSAFE_TEMP_CHECK = r"""
import importlib.util
import os
from pathlib import Path
import shutil
import sys
import tempfile
from unittest.mock import patch

source, server_text, backup_text, location_text, mode = sys.argv[1:]
if "PYTHONOPTIMIZE" in os.environ:
    raise AssertionError("PYTHONOPTIMIZE must not reach the child process")
spec = importlib.util.spec_from_file_location("isolated_restore_verifier", source)
verifier = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verifier)
backup = Path(backup_text).resolve(strict=True)
location = Path(location_text).resolve(strict=True)
real_open = os.open
old_tempdir = tempfile.tempdir

def is_inside_backup(path):
    return Path(os.fsdecode(path)).resolve().is_relative_to(backup)

def guard_open(path, flags, *args, **kwargs):
    if flags & os.O_CREAT and is_inside_backup(path):
        raise AssertionError("blocked unsafe temporary-file probe inside backup")
    return real_open(path, flags, *args, **kwargs)

def guard_mkdir(path, *args, **kwargs):
    if is_inside_backup(path):
        raise AssertionError("blocked directory creation inside backup")
    raise AssertionError("scratch allocation was attempted before rejection")

def block_copy(*args, **kwargs):
    raise AssertionError("backup copy was attempted before rejection")

def block_native(*args, **kwargs):
    raise AssertionError("native verifier was started before rejection")

try:
    tempfile.tempdir = None if mode == "fresh" else str(location)
    with patch.object(os, "open", side_effect=guard_open), \
         patch.object(os, "mkdir", side_effect=guard_mkdir), \
         patch.object(shutil, "copytree", side_effect=block_copy), \
         patch.object(verifier.subprocess, "run", side_effect=block_native):
        try:
            verifier.verify_restore_point(Path(server_text), backup)
        except ValueError as error:
            message = str(error).lower()
            if "temporary directory" not in message or "backup" not in message:
                raise AssertionError("unexpected rejection message: " + message)
            print("REJECTED:", type(error).__name__, str(error))
        else:
            raise AssertionError("unsafe temporary directory was not rejected")
finally:
    tempfile.tempdir = old_tempdir
"""


# Exercise a fresh process and real temporary-tree copy without relying on the
# global tempfile cache or its probing gettempdir() implementation.
_SAFE_TEMP_CHECK = r"""
from contextlib import nullcontext
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from unittest.mock import patch

source, server_text, backup_text, safe_text, loop_text = sys.argv[1:]
if "PYTHONOPTIMIZE" in os.environ:
    raise AssertionError("PYTHONOPTIMIZE must not reach the child process")
spec = importlib.util.spec_from_file_location("isolated_restore_verifier", source)
verifier = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verifier)
server = Path(server_text)
backup = Path(backup_text).resolve(strict=True)
safe = Path(safe_text).resolve(strict=True)
old_tempdir = tempfile.tempdir
observed = []
injected = []
real_resolve = Path.resolve

def resolve_candidate(path, *args, **kwargs):
    if loop_text and path == Path(loop_text):
        injected.append(True)
        raise RuntimeError("simulated symlink loop")
    return real_resolve(path, *args, **kwargs)

def inspect_native(argv, **kwargs):
    scratch = Path(kwargs["cwd"]).resolve(strict=True)
    target = Path(argv[2]).resolve(strict=True)
    if scratch.parent != safe or scratch.is_relative_to(backup):
        raise AssertionError("scratch must be inside safe temp and outside backup")
    if target != scratch / "world" / "flat.irw":
        raise AssertionError("native verifier must receive the isolated copy")
    if target.read_bytes() != (backup / "flat.irw").read_bytes():
        raise AssertionError("isolated copy differs from backup")
    if argv[:2] != [str(server.resolve(strict=True)), "--verify-world"]:
        raise AssertionError("native verifier executable or command mismatch")
    if kwargs["check"] is not True or kwargs["timeout"] != 30:
        raise AssertionError("native verifier check or timeout mismatch")
    observed.append(scratch)
    return subprocess.CompletedProcess(argv, 0)

try:
    tempfile.tempdir = None
    loop_patch = patch.object(Path, "resolve", new=resolve_candidate) if loop_text else nullcontext()
    with loop_patch, \
         patch.object(tempfile, "gettempdir", side_effect=AssertionError("unexpected gettempdir probe")), \
         patch.object(verifier.subprocess, "run", side_effect=inspect_native):
        count = verifier.verify_restore_point(server, backup, timeout=30)
    if loop_text and not injected:
        raise AssertionError("symlink-loop error was not injected")
    if count != 3 or len(observed) != 1:
        raise AssertionError("restore did not inspect the expected files exactly once")
    if observed[0].exists():
        raise AssertionError("scratch directory was not cleaned up")
    print("VERIFIED: scratch created and removed outside backup")
finally:
    tempfile.tempdir = old_tempdir
"""


class RestoreDrillTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="j92-drill-test-")
        self.addCleanup(self.tmp.cleanup)
        self.base = Path(self.tmp.name)
        self.backup = self.base / "backup"
        self.backup.mkdir()
        self.server = self.base / "server"
        self.server.write_bytes(b"native executable fixture")
        (self.backup / ".industrial1710-backup").write_bytes(module.MARKER)
        (self.backup / "flat.irw").write_bytes(b"IRW3 fixture")
        (self.backup / "players").mkdir()
        (self.backup / "players" / "user.dat").write_bytes(b"player")

    def test_reparse_points_are_rejected_before_copy_or_native_verification(self):
        # A Windows junction reports S_ISDIR even when lstat does not follow it.
        # Inject the attribute independently of platform and include the marker,
        # which is read before the normal tree scan.
        real_lstat = Path.lstat
        real_open = Path.open
        for target in (self.backup, self.backup / ".industrial1710-backup",
                       self.backup / "players", self.backup / "players" / "user.dat"):
            with self.subTest(target=target):
                def lstat_with_reparse(path, *args, **kwargs):
                    metadata = real_lstat(path, *args, **kwargs)
                    if path == target:
                        return SimpleNamespace(st_mode=metadata.st_mode, st_size=metadata.st_size,
                                               st_file_attributes=0x400)
                    return metadata

                def guard_open(path, *args, **kwargs):
                    if path == target or path.is_relative_to(target):
                        raise AssertionError("read crossed a reparse point")
                    return real_open(path, *args, **kwargs)

                with patch.object(Path, "lstat", new=lstat_with_reparse), \
                     patch.object(Path, "open", new=guard_open), \
                     patch.object(module.shutil, "copytree") as copytree, \
                     patch.object(module.subprocess, "run") as native:
                    with self.assertRaisesRegex(ValueError, "reparse"):
                        module.verify_restore_point(self.server, self.backup)
                    copytree.assert_not_called()
                    native.assert_not_called()

    def test_windows_symlink_keeps_the_existing_link_diagnostic(self):
        target = self.backup / "players" / "user.dat"
        real_lstat = Path.lstat

        def lstat_with_windows_symlink(path, *args, **kwargs):
            metadata = real_lstat(path, *args, **kwargs)
            if path == target:
                return SimpleNamespace(st_mode=stat.S_IFLNK | 0o777,
                                       st_size=metadata.st_size, st_file_attributes=0x400)
            return metadata

        with patch.object(Path, "lstat", new=lstat_with_windows_symlink):
            with self.assertRaisesRegex(ValueError, "symlink"):
                module.verify_restore_point(self.server, self.backup)

    def test_level_dat_reparse_point_is_rejected_before_format_detection(self):
        target = self.backup / "level.dat"
        target.write_bytes(b"synthetic")
        real_lstat = Path.lstat
        real_is_file = Path.is_file

        def lstat_with_reparse(path, *args, **kwargs):
            metadata = real_lstat(path, *args, **kwargs)
            if path == target:
                return SimpleNamespace(st_mode=metadata.st_mode, st_size=metadata.st_size,
                                       st_file_attributes=0x400)
            return metadata

        def guard_is_file(path):
            if path == target:
                raise AssertionError("format detection followed a reparse point")
            return real_is_file(path)

        with patch.object(Path, "lstat", new=lstat_with_reparse), \
             patch.object(Path, "is_file", new=guard_is_file):
            with self.assertRaisesRegex(ValueError, "reparse"):
                module.verify_restore_point(self.server, self.backup)

    def test_marker_check_is_bounded_and_handles_short_reads(self):
        marker = self.backup / ".industrial1710-backup"

        class ShortReader(io.BytesIO):
            def __init__(self, content):
                super().__init__(content)
                self.returned = 0

            def read(self, size=-1):
                if size < 0:
                    raise AssertionError("unbounded marker read")
                result = super().read(min(size, 3))
                self.returned += len(result)
                if self.returned > len(module.MARKER) + 1:
                    raise AssertionError("marker read exceeded bound")
                return result

        for content, expected in ((module.MARKER, True), (module.MARKER + b"x" * 10000, False),
                                  (module.MARKER[:-1], False), (b"", False)):
            with self.subTest(length=len(content)):
                stream = ShortReader(content)
                with patch.object(Path, "open", return_value=stream):
                    self.assertEqual(module._marker_matches(marker), expected)
                self.assertLessEqual(stream.returned, len(module.MARKER) + 1)

    def test_marker_read_error_fails_closed(self):
        marker = self.backup / ".industrial1710-backup"
        with patch.object(Path, "open", side_effect=OSError("synthetic read error")):
            with self.assertRaises(OSError):
                module._marker_matches(marker)

        class WouldBlock(io.BytesIO):
            def read(self, size=-1):
                return None

        with patch.object(Path, "open", return_value=WouldBlock()):
            with self.assertRaises(BlockingIOError):
                module._marker_matches(marker)

    def test_oversized_marker_is_rejected_before_tree_scan(self):
        marker = self.backup / ".industrial1710-backup"
        marker.write_bytes(module.MARKER + b"x" * (1024 * 1024))
        with patch.object(module, "snapshot_hashes") as scan, \
             patch.object(module.shutil, "copytree") as copytree, \
             patch.object(module.subprocess, "run") as native:
            with self.assertRaisesRegex(ValueError, "marker"):
                module.verify_restore_point(self.server, self.backup)
            scan.assert_not_called()
            copytree.assert_not_called()
            native.assert_not_called()

    @unittest.skipUnless(os.name == "nt", "Windows junction test")
    def test_windows_junction_is_rejected_without_reading_target(self):
        outside = self.base / "outside"
        outside.mkdir()
        sentinel = outside / "synthetic.txt"
        sentinel.write_bytes(b"external synthetic fixture")
        junction = self.backup / "junction"
        env = os.environ.copy()
        env["RESTORE_TEST_JUNCTION"] = str(junction)
        env["RESTORE_TEST_TARGET"] = str(outside)
        result = subprocess.run(
            ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command",
             "New-Item -ItemType Junction -Path $env:RESTORE_TEST_JUNCTION "
             "-Target $env:RESTORE_TEST_TARGET -ErrorAction Stop | Out-Null"],
            env=env, capture_output=True, check=False,
        )
        if result.returncode:
            self.skipTest("cannot create Windows junction in this environment")
        real_open = Path.open

        def guard_open(path, *args, **kwargs):
            if path == junction or path.is_relative_to(junction):
                raise AssertionError("read crossed a junction")
            return real_open(path, *args, **kwargs)

        with patch.object(Path, "open", new=guard_open), \
             patch.object(module.shutil, "copytree") as copytree, \
             patch.object(module.subprocess, "run") as native:
            with self.assertRaisesRegex(ValueError, "reparse"):
                module.verify_restore_point(self.server, self.backup)
            copytree.assert_not_called()
            native.assert_not_called()
        self.assertEqual(sentinel.read_bytes(), b"external synthetic fixture")

    @patch.object(module.subprocess, "run")
    def test_drill_uses_isolated_copy_then_disposes_it(self, run):
        original = module.snapshot_hashes(self.backup)
        self.assertEqual(module.verify_restore_point(self.server, self.backup), 3)
        run.assert_called_once()
        argv = run.call_args.args[0]
        self.assertEqual(argv[:2], [str(self.server.resolve(strict=True)), "--verify-world"])
        self.assertNotEqual(Path(argv[2]), self.backup)
        self.assertFalse(Path(argv[2]).exists())
        self.assertEqual(module.snapshot_hashes(self.backup), original)
        self.assertTrue(run.call_args.kwargs["check"])


    @patch.object(module.subprocess, "run")
    def test_anvil_world_and_verifier_cwd_remain_inside_scratch(self, run):
        (self.backup / "flat.irw").unlink()
        (self.backup / "level.dat").write_bytes(b"anvil fixture")
        before = module.snapshot_hashes(self.backup)
        self.assertEqual(module.verify_restore_point(self.server, self.backup), 3)
        argv = run.call_args.args[0]
        cwd = Path(run.call_args.kwargs["cwd"])
        target = Path(argv[2])
        self.assertEqual(Path(argv[0]), self.server.resolve())
        self.assertEqual(target, cwd / "world")
        self.assertNotEqual(cwd, self.base)
        self.assertFalse(cwd.exists(), "scratch working directory must be disposed")
        self.assertEqual(module.snapshot_hashes(self.backup), before)

    @patch.object(module.subprocess, "run")
    def test_relative_server_binary_remains_resolvable_with_isolated_cwd(self, run):
        import os
        previous = Path.cwd()
        try:
            os.chdir(self.base)
            self.assertEqual(module.verify_restore_point(Path("server"), Path("backup")), 3)
            self.assertEqual(Path(run.call_args.args[0][0]), self.server.resolve())
            self.assertNotEqual(Path(run.call_args.kwargs["cwd"]), self.base)
        finally:
            os.chdir(previous)

    @patch.object(module.subprocess, "run")
    def test_nonflat_irw_basename_is_verified_as_a_file(self, run):
        (self.backup / "flat.irw").rename(self.backup / "experiment.irw")
        self.assertEqual(module.verify_restore_point(self.server, self.backup), 3)
        target = Path(run.call_args.args[0][2])
        self.assertEqual(target.name, "experiment.irw")
        self.assertNotEqual(target.parent, self.backup)

    @patch.object(module.subprocess, "run")
    def test_multiple_irw_candidates_are_rejected(self, run):
        (self.backup / "second.irw").write_bytes(b"other")
        with self.assertRaisesRegex(ValueError, "ambiguous"):
            module.verify_restore_point(self.server, self.backup)
        run.assert_not_called()

    @patch.object(module.subprocess, "run")
    def test_unmarked_backup_is_rejected_before_copy(self, run):
        (self.backup / ".industrial1710-backup").unlink()
        with self.assertRaises(OSError):
            module.verify_restore_point(self.server, self.backup)
        run.assert_not_called()

    @patch.object(module.subprocess, "run")
    def test_modified_marker_never_claims_restore_pass(self, run):
        (self.backup / ".industrial1710-backup").write_bytes(b"forged")
        with self.assertRaisesRegex(ValueError, "marker"):
            module.verify_restore_point(self.server, self.backup)
        run.assert_not_called()

    @patch.object(module.subprocess, "run")
    def test_verifier_must_not_modify_restored_scratch_copy(self, run):
        original = module.snapshot_hashes(self.backup)

        def mutate_copy(argv, **_kwargs):
            # The fixture is a legitimate child of the isolated destination.
            restored_world = Path(argv[2]).parent
            (restored_world / "players" / "user.dat").write_bytes(b"corrupted")
            return subprocess.CompletedProcess(argv, 0)

        run.side_effect = mutate_copy
        with self.assertRaisesRegex(ValueError, "modified the isolated restored world"):
            module.verify_restore_point(self.server, self.backup)
        self.assertEqual(module.snapshot_hashes(self.backup), original)

    @patch.object(module.subprocess, "run")
    def test_verifier_failure_propagates_and_source_remains(self, run):
        run.side_effect = subprocess.CalledProcessError(1, "bad-world")
        with self.assertRaises(subprocess.CalledProcessError):
            module.verify_restore_point(self.server, self.backup)
        self.assertTrue((self.backup / "flat.irw").is_file())

    def _assert_unsafe_temp_rejected(self, mode, env_name, location):
        before_hashes = module.snapshot_hashes(self.backup)
        paths = [self.backup, *sorted(self.backup.rglob("*"))]
        before_metadata = [(p.relative_to(self.backup).as_posix(), p.stat().st_mode,
                            p.stat().st_size, p.stat().st_mtime_ns) for p in paths]
        environment = os.environ.copy()
        environment.pop("PYTHONOPTIMIZE", None)
        for key in ("TMPDIR", "TEMP", "TMP"):
            environment.pop(key, None)
        if env_name is not None:
            environment[env_name] = str(location)
        result = subprocess.run(
            [sys.executable, "-c", _UNSAFE_TEMP_CHECK, str(Path(module.__file__)),
             str(self.server), str(self.backup), str(location), mode],
            env=environment, capture_output=True, text=True, timeout=30, check=False,
        )
        after_paths = [self.backup, *sorted(self.backup.rglob("*"))]
        after_metadata = [(p.relative_to(self.backup).as_posix(), p.stat().st_mode,
                           p.stat().st_size, p.stat().st_mtime_ns) for p in after_paths]
        self.assertEqual(module.snapshot_hashes(self.backup), before_hashes)
        self.assertEqual(after_metadata, before_metadata)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("REJECTED:", result.stdout)

    def test_uncached_temp_environment_inside_backup_rejected_before_probe(self):
        nested = self.backup / "temp-location"
        nested.mkdir()
        for env_name in ("TMPDIR", "TEMP", "TMP"):
            for location in (self.backup, nested):
                with self.subTest(env_name=env_name, location=location):
                    self._assert_unsafe_temp_rejected("fresh", env_name, location)

    def test_cached_tempdir_inside_backup_rejected_before_scratch(self):
        nested = self.backup / "temp-location"
        nested.mkdir()
        for location in (self.backup, nested):
            with self.subTest(location=location):
                self._assert_unsafe_temp_rejected("cached", None, location)

    def _assert_safe_temp_accepted(self, environment, loop_path=None):
        before_hashes = module.snapshot_hashes(self.backup)
        safe = self.base / "safe-temp"
        safe.mkdir()
        environment = {**os.environ, **environment}
        environment.pop("PYTHONOPTIMIZE", None)
        for key in ("TMPDIR", "TEMP", "TMP"):
            if environment.get(key) is None:
                environment.pop(key, None)
        result = subprocess.run(
            [sys.executable, "-c", _SAFE_TEMP_CHECK, str(Path(module.__file__)),
             str(self.server), str(self.backup), str(safe), str(loop_path) if loop_path else ""],
            env=environment, capture_output=True, text=True, timeout=30, check=False,
        )
        self.assertEqual(module.snapshot_hashes(self.backup), before_hashes)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("VERIFIED:", result.stdout)
        self.assertEqual(list(safe.iterdir()), [], "temporary directory must be empty after cleanup")

    def test_uncached_safe_tmpdir_permits_restore_without_global_probe(self):
        self._assert_safe_temp_accepted({
            "TMPDIR": str(self.base / "safe-temp"), "TEMP": None, "TMP": None,
        })

    def test_invalid_tmpdir_falls_back_to_safe_temp_without_global_probe(self):
        invalid = self.base / "does-not-exist"
        self._assert_safe_temp_accepted({
            "TMPDIR": str(invalid), "TEMP": str(self.base / "safe-temp"), "TMP": None,
        })
        self.assertFalse(invalid.exists())

    def test_regular_file_in_backup_falls_back_to_safe_temp_without_global_probe(self):
        self._assert_safe_temp_accepted({
            "TMPDIR": str(self.backup / "flat.irw"),
            "TEMP": str(self.base / "safe-temp"),
            "TMP": None,
        })

    def test_resolve_runtime_error_falls_back_to_safe_temp_without_global_probe(self):
        loop_path = self.base / "simulated-loop"
        self._assert_safe_temp_accepted({
            "TMPDIR": str(loop_path), "TEMP": str(self.base / "safe-temp"), "TMP": None,
        }, loop_path=loop_path)
        self.assertFalse(loop_path.exists())

    def test_unavailable_temp_candidates_do_not_fallback_to_cwd(self):
        before = module.snapshot_hashes(self.backup)
        previous_cwd = Path.cwd()
        environment = {key: str(self.base) for key in ("TMPDIR", "TEMP", "TMP")}
        try:
            os.chdir(self.backup)
            with patch.dict(os.environ, environment), \
                 patch.object(tempfile, "tempdir", None), \
                 patch.object(tempfile, "TemporaryDirectory", side_effect=OSError("unavailable")) as scratch, \
                 patch.object(os, "mkdir", side_effect=AssertionError("unexpected directory creation")) as mkdir, \
                 patch.object(module.shutil, "copytree") as copy, \
                 patch.object(module.subprocess, "run") as native:
                with self.assertRaisesRegex(OSError, "no usable temporary directory outside the backup"):
                    module.verify_restore_point(self.server, self.backup)
                self.assertGreater(scratch.call_count, 0)
                attempted = [Path(call.kwargs["dir"]).resolve() for call in scratch.call_args_list]
                self.assertNotIn(self.backup.resolve(), attempted)
                mkdir.assert_not_called()
                copy.assert_not_called()
                native.assert_not_called()
        finally:
            os.chdir(previous_cwd)
        self.assertEqual(module.snapshot_hashes(self.backup), before)

    @unittest.skipUnless(hasattr(__import__("os"), "symlink"), "symlink not available")
    @patch.object(module.subprocess, "run")
    def test_symlink_is_rejected_without_reading_target(self, run):
        import os
        try:
            os.symlink(self.server, self.backup / "bad-link")
        except (OSError, NotImplementedError):
            self.skipTest("cannot create symlink")
        with self.assertRaisesRegex(ValueError, "symlink"):
            module.verify_restore_point(self.server, self.backup)
        run.assert_not_called()


if __name__ == "__main__":
    unittest.main()

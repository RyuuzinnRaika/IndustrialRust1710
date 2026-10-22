"""Restore drills never mutate the original, trust an unmarked tree, or claim GUI acceptance."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "verify_backup_restore", Path(__file__).with_name("verify_backup_restore.py")
)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


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

    @patch.object(module.subprocess, "run")
    def test_drill_uses_isolated_copy_then_disposes_it(self, run):
        original = module.snapshot_hashes(self.backup)
        self.assertEqual(module.verify_restore_point(self.server, self.backup), 3)
        run.assert_called_once()
        argv = run.call_args.args[0]
        self.assertEqual(argv[:2], [str(self.server), "--verify-world"])
        self.assertNotEqual(Path(argv[2]), self.backup)
        self.assertFalse(Path(argv[2]).exists())
        self.assertEqual(module.snapshot_hashes(self.backup), original)
        self.assertTrue(run.call_args.kwargs["check"])

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
    def test_verifier_failure_propagates_and_source_remains(self, run):
        run.side_effect = subprocess.CalledProcessError(1, "bad-world")
        with self.assertRaises(subprocess.CalledProcessError):
            module.verify_restore_point(self.server, self.backup)
        self.assertTrue((self.backup / "flat.irw").is_file())

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

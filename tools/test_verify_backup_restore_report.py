"""Issue #523: offline restore reports must be accurate and read-only."""
import contextlib
from io import StringIO
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import verify_backup_restore_report as report


class BackupReportTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ir1710-report-")
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.backup = self.base / "backup-12345-99"
        self.backup.mkdir()
        self.server = self.base / "server"
        self.server.write_bytes(b"test native binary placeholder")
        (self.backup / ".industrial1710-backup").write_bytes(report.verifier.MARKER)
        (self.backup / "flat.irw").write_bytes(b"IRW1\0\0\0\0")
        (self.backup / "players").mkdir()
        (self.backup / "players" / "alex.dat").write_bytes(b"player")

    @patch.object(report.verifier.subprocess, "run")
    def test_report_lists_files_bytes_and_checksums_after_isolated_restore(self, run):
        before = report.verifier.snapshot_hashes(self.backup)
        value = report.report_restore_point(self.server, self.backup)
        self.assertEqual(value["status"], "PASS")
        self.assertEqual(value["world_format"], "IRW")
        self.assertEqual(value["file_count"], 3)
        self.assertEqual(value["backup_generation_directory"], "backup-12345-99")
        self.assertEqual(value["total_bytes"], len(report.verifier.MARKER) + 8 + 6)
        self.assertEqual([v["path"] for v in value["files"]],
                         [".industrial1710-backup", "flat.irw", "players/alex.dat"])
        self.assertEqual([v["sha256"] for v in value["files"]],
                         [before[v["path"]] for v in value["files"]])
        self.assertEqual(len(value["manifest_sha256"]), 64)
        self.assertEqual(report.verifier.snapshot_hashes(self.backup), before)
        self.assertTrue(value["source_bytes_unchanged"])
        command = run.call_args.args[0]
        self.assertEqual(command[:2], [str(self.server), "--verify-world"])
        self.assertNotEqual(Path(command[2]), self.backup / "flat.irw")
        self.assertFalse(Path(command[2]).exists(), "scratch tree must be disposed")

    @patch.object(report.verifier.subprocess, "run")
    def test_report_is_deterministic_and_json_serializable(self, run):
        first = report.report_restore_point(self.server, self.backup)
        second = report.report_restore_point(self.server, self.backup)
        self.assertEqual(first, second)
        self.assertEqual(json.loads(json.dumps(first)), first)
        self.assertEqual(run.call_count, 2)

    @patch.object(report.verifier.subprocess, "run")
    def test_backup_change_after_restore_is_rejected(self, run):
        original = report.verifier.verify_restore_point

        def tamper(server, backup, timeout):
            count = original(server, backup, timeout)
            (backup / "flat.irw").write_bytes(b"IRW5-CHANGED")
            return count

        with patch.object(report.verifier, "verify_restore_point", side_effect=tamper):
            with self.assertRaisesRegex(ValueError, "content changed"):
                report.report_restore_point(self.server, self.backup)

    @patch.object(report.verifier.subprocess, "run")
    def test_native_verification_failure_prevents_pass_report(self, run):
        run.side_effect = subprocess.CalledProcessError(4, "server")
        with self.assertRaises(subprocess.CalledProcessError):
            report.report_restore_point(self.server, self.backup)
        self.assertTrue((self.backup / "flat.irw").exists())

    @patch.object(report.verifier.subprocess, "run")
    def test_invalid_ownership_marker_is_rejected(self, run):
        (self.backup / ".industrial1710-backup").write_bytes(b"bogus")
        with self.assertRaisesRegex(ValueError, "marker"):
            report.report_restore_point(self.server, self.backup)
        run.assert_not_called()

    @patch.object(report.verifier.subprocess, "run")
    def test_cli_outputs_json_only_on_success(self, run):
        output = StringIO()
        with contextlib.redirect_stdout(output):
            code = report.main(["--server", str(self.server), "--backup", str(self.backup)])
        self.assertEqual(code, 0)
        self.assertEqual(json.loads(output.getvalue())["file_count"], 3)

    @patch.object(report.verifier.subprocess, "run")
    def test_cli_exits_nonzero_without_success_json_on_failure(self, run):
        output = StringIO()
        error = StringIO()
        (self.backup / ".industrial1710-backup").unlink()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(error):
            code = report.main(["--server", str(self.server), "--backup", str(self.backup)])
        self.assertEqual(code, 1)
        self.assertEqual(output.getvalue(), "")
        self.assertIn("RESTORE_REPORT_FAILED", error.getvalue())
        run.assert_not_called()


if __name__ == "__main__":
    unittest.main()

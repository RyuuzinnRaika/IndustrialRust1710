"""Issue #512: representative corruption fixtures and failure conditions."""
import hashlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import contextlib

import save_fixture_report as scan


class SaveFixtureTests(unittest.TestCase):
    def setUp(self):
        self.t = tempfile.TemporaryDirectory(prefix="ir1710-save-fixture-tests-")
        self.addCleanup(self.t.cleanup)
        self.root = Path(self.t.name)
        self.target = self.root / "fixture.irw"

    def check_fixture(self, data, expected):
        self.target.write_bytes(data)
        before = hashlib.sha256(data).hexdigest()
        result = scan.inspect(self.target)
        self.assertEqual(result["classification"], expected)
        self.assertEqual(result["sha256"], before)
        self.assertTrue(result["original_unchanged"])
        self.assertEqual(self.target.read_bytes(), data)
        self.assertEqual(result["native_verifier"], "NOT_RUN")
        return result

    def test_minimal_irw1_requires_native_validation(self):
        result = self.check_fixture(b"IRW1\x00\x00\x00\x00", "requires_native_verification")
        self.assertEqual(result["repair_action"], "run_native_verifier")

    def test_corrupt_header_not_repaired(self):
        result = self.check_fixture(b"XXXX\x00\x00\x00\x00", "invalid_magic")
        self.assertEqual(result["repair_action"], "restore_from_verified_backup_only")

    def test_truncated_irw1_header(self):
        self.check_fixture(b"IRW1\x00", "truncated_header")

    def test_unknown_version_requires_manual_review(self):
        result = self.check_fixture(b"IRW5\x00\x00\x00\x00", "unsupported_version")
        self.assertEqual(result["repair_action"], "manual_review_unknown_version")

    def test_edit_count_requiring_more_data_is_truncated(self):
        self.check_fixture(b"IRW1\x00\x00\x00\x01", "truncated_payload")

    def test_edit_count_is_bounded(self):
        self.check_fixture(b"IRW1\x00\x01\x00\x01", "invalid_edit_count")

    def test_irw4_invalid_dimension(self):
        self.check_fixture(b"IRW4" + (0).to_bytes(8, "big") + b"\x09" + (0).to_bytes(4, "big"), "invalid_dimension")

    def test_irw4_valid_dimension_still_needs_native(self):
        self.check_fixture(b"IRW4" + (0).to_bytes(8, "big") + b"\xff" + (0).to_bytes(4, "big"), "requires_native_verification")

    def test_manifest_checksum_mismatch_fails_closed(self):
        self.target.write_bytes(b"IRW1\x00\x00\x00\x00")
        report = scan.inspect(self.target, expected_sha256="f" * 64)
        self.assertEqual(report["classification"], "checksum_mismatch")
        self.assertEqual(report["repair_action"], "restore_from_verified_backup_only")

    def test_manifest_checksum_match_is_case_insensitive(self):
        data = b"IRW1\x00\x00\x00\x00"
        self.target.write_bytes(data)
        report = scan.inspect(self.target, expected_sha256=hashlib.sha256(data).hexdigest().upper())
        self.assertEqual(report["classification"], "requires_native_verification")

    def test_reject_symlink_even_with_valid_target(self):
        self.target.write_bytes(b"IRW1\x00\x00\x00\x00")
        link = self.root / "link.irw"
        try:
            link.symlink_to(self.target)
        except (OSError, NotImplementedError):
            self.skipTest("symlinks not available")
        with self.assertRaisesRegex(scan.InspectionFailure, "regular file"):
            scan.inspect(link)
        self.assertTrue(self.target.exists())

    @patch.object(scan.subprocess, "run")
    def test_native_verifier_receives_temporary_copy_not_original(self, runner):
        server = self.root / "server.bin"
        server.write_bytes(b"placeholder")
        data = b"IRW1\x00\x00\x00\x00"
        self.target.write_bytes(data)
        runner.return_value.returncode = 0
        report = scan.inspect(self.target, server=server)
        self.assertEqual(report["classification"], "native_verified")
        self.assertEqual(report["native_verifier"], "PASS")
        argv = runner.call_args.args[0]
        self.assertEqual(argv[1], "--verify-world")
        self.assertNotEqual(Path(argv[2]), self.target)
        self.assertFalse(Path(argv[2]).exists())
        self.assertEqual(self.target.read_bytes(), data)

    @patch.object(scan.subprocess, "run")
    def test_native_rejection_means_restore_from_backup_not_auto_repair(self, runner):
        server = self.root / "server.bin"
        server.write_bytes(b"placeholder")
        self.target.write_bytes(b"IRW1\x00\x00\x00\x00")
        runner.return_value.returncode = 1
        report = scan.inspect(self.target, server=server)
        self.assertEqual(report["classification"], "native_failed")
        self.assertEqual(report["repair_action"], "restore_from_verified_backup_only")

    def test_cli_emits_json_not_destructive_write(self):
        data = b"IRW5\x00\x00\x00\x00"
        self.target.write_bytes(data)
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = scan.main(["--file", str(self.target)])
        self.assertEqual(code, 0)
        self.assertEqual(json.loads(output.getvalue())["classification"], "unsupported_version")
        self.assertEqual(self.target.read_bytes(), data)

    def test_bad_expected_hash_rejected(self):
        self.target.write_bytes(b"IRW1\x00\x00\x00\x00")
        with self.assertRaises(scan.InspectionFailure):
            scan.inspect(self.target, expected_sha256="abc")


if __name__ == "__main__":
    unittest.main()

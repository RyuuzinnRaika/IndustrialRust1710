"""Offline checks for the REG-200 Windows/Linux evidence comparator."""
import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest

import verify_local_report_pair as pair


class PairReportTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="ir1710-reg200-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)

    def report(self, os_name, status="PASS", commit="a" * 40):
        return {
            "schema_version": 1,
            "commit": commit,
            "host_os": os_name,
            "host_arch": "x86_64" if os_name == "linux" else "x86_64",
            "exit_code": 0 if status == "PASS" else 1,
            "checks": [
                {"name": name, "status": "NOT_RUN" if name.startswith("E42") else status,
                 "command": "private optional details",
                 "exit_code": None if name.startswith("E42") else (0 if status == "PASS" else 1),
                 "log": "private/path"}
                for name in pair.CHECK_NAMES
            ],
        }

    def write(self, name, data):
        path = self.root / name
        path.write_text(json.dumps(data), encoding="utf-8")
        return path

    def test_same_commit_and_check_statuses_match(self):
        win = pair.load_report(self.write("win.json", self.report("windows")), "windows")
        lin = pair.load_report(self.write("lin.json", self.report("linux")), "linux")
        summary = pair.compare_reports(win, lin)
        self.assertTrue(summary["identical_statuses"])
        self.assertEqual(len(summary["checks"]), 7)
        self.assertNotIn("private", json.dumps(summary))
        self.assertEqual(summary["checks"][-1]["windows"], "NOT_RUN")

    def test_mismatch_exits_one_and_output_is_create_only(self):
        w = self.write("win.json", self.report("windows"))
        alternate = self.report("linux")
        alternate["checks"][0]["status"] = "FAIL"
        alternate["checks"][0]["exit_code"] = 101
        alternate["exit_code"] = 1
        l = self.write("lin.json", alternate)
        output = self.root / "comparison.json"
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(pair.main(["--windows", str(w), "--ubuntu", str(l), "--output", str(output)]), 1)
            self.assertEqual(pair.main(["--windows", str(w), "--ubuntu", str(l), "--output", str(output)]), 2)
        result = json.loads(output.read_text(encoding="utf-8"))
        self.assertFalse(result["identical_statuses"])
        self.assertEqual(result["checks"][0]["linux"], "FAIL")

    def test_matching_failed_verifiers_are_not_reported_as_success(self):
        w = self.write("win.json", self.report("windows", status="FAIL"))
        l = self.write("lin.json", self.report("linux", status="FAIL"))
        with contextlib.redirect_stdout(io.StringIO()) as shown:
            self.assertEqual(pair.main(["--windows", str(w), "--ubuntu", str(l)]), 1)
        summary = json.loads(shown.getvalue())
        self.assertTrue(summary["identical_statuses"])
        self.assertFalse(summary["both_verifiers_succeeded"])

    def test_invalid_exit_code_status_combinations_are_rejected(self):
        cases = [
            ("PASS", None, "PASS without zero"),
            ("PASS", 1, "PASS without zero"),
            ("FAIL", 0, "FAIL with zero"),
            ("NOT_RUN", 0, "NOT_RUN with an exit_code"),
            ("PASS", [0], "malformed check exit_code"),
        ]
        for index, (status, code, error) in enumerate(cases):
            invalid = self.report("windows")
            invalid["checks"][0]["status"] = status
            invalid["checks"][0]["exit_code"] = code
            with self.subTest(index=index), self.assertRaisesRegex(pair.EvidenceError, error):
                pair.load_report(self.write(f"invalid_exit_{index}.json", invalid), "windows")

    def test_different_commit_is_rejected(self):
        win = self.report("windows")
        lin = self.report("linux", commit="b" * 40)
        with self.assertRaisesRegex(pair.EvidenceError, "different Git commits"):
            pair.compare_reports(win, lin)

    def test_wrong_os_and_unknown_commit_are_rejected(self):
        win = self.write("win.json", self.report("linux"))
        with self.assertRaisesRegex(pair.EvidenceError, "host_os"):
            pair.load_report(win, "windows")
        invalid = self.report("windows", commit="unknown")
        with self.assertRaisesRegex(pair.EvidenceError, "full Git SHA"):
            pair.load_report(self.write("unknown.json", invalid), "windows")

    def test_missing_check_and_unexpected_status_are_rejected(self):
        truncated = self.report("windows")
        truncated["checks"].pop()
        with self.assertRaisesRegex(pair.EvidenceError, "missing or extra"):
            pair.load_report(self.write("truncated.json", truncated), "windows")
        broken = self.report("windows")
        broken["checks"][0]["status"] = "SKIPPED"
        with self.assertRaisesRegex(pair.EvidenceError, "invalid check status"):
            pair.load_report(self.write("broken.json", broken), "windows")

    def test_nested_unhashable_status_is_rejected_without_traceback(self):
        invalid = self.report("windows")
        invalid["checks"][0]["status"] = ["PASS"]
        with self.assertRaisesRegex(pair.EvidenceError, "invalid check status"):
            pair.load_report(self.write("nested_status.json", invalid), "windows")

    def test_oversized_report_is_rejected_without_reading_logs(self):
        path = self.root / "oversized.json"
        path.write_bytes(b" " * (pair.MAX_REPORT_BYTES + 1))
        with self.assertRaisesRegex(pair.EvidenceError, "exceeds"):
            pair.load_report(path, "windows")

    def test_never_overwrite_input(self):
        w = self.write("win.json", self.report("windows"))
        l = self.write("lin.json", self.report("linux"))
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(pair.main(["--windows", str(w), "--ubuntu", str(l), "--output", str(w)]), 2)
        self.assertEqual(pair.load_report(w, "windows")["commit"], "a" * 40)


if __name__ == "__main__":
    unittest.main()

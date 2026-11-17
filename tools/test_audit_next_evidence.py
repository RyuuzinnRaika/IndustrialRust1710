"""Offline regression tests for NEXT evidence audit and real-client comparison."""
import tempfile
import unittest
from pathlib import Path

import audit_next_evidence as audit
from e42_acceptance import CASES, new_report

SHA = "a" * 40


class EvidenceAuditTests(unittest.TestCase):
    def test_closed_and_open_issues_are_scanned_with_missing_evidence(self):
        issues = [{"number": 123, "title": "closed", "state": "closed", "body": "missing"},
                  {"number": 124, "title": "open", "state": "open",
                   "body": "cargo test -p vanilla-server --test protocol5_e2e\nprotocol5_e2e::join_test\n" + SHA}]
        prs = [{"number": 125, "state": "open", "body": "関連要件: M2/D31\n"
                + SHA + "\nprotocol5_e2e::join_test\nclient E42 PASS"}]
        result = audit.audit_snapshot(issues, prs)
        self.assertEqual(result["counts"]["closed_issues_with_gaps"], 1)
        self.assertIn("named_test", result["issues"][0]["missing"])
        self.assertEqual(result["prs"][0]["requirements"], ["D31", "M2"])
        self.assertEqual(result["prs"][0]["client_status"], "NOT_RUN")
        self.assertNotIn("requirement_ids", result["prs"][0]["missing"])

    def test_missing_or_unspecified_fields_do_not_become_pass(self):
        result = audit.audit_snapshot([{"number": 1, "state": "closed", "body": "E42 pass?"}], [])
        self.assertEqual(result["issues"][0]["client_status"], "NOT_RUN")
        self.assertEqual(len(result["issues"][0]["missing"]), 3)
        with self.assertRaises(ValueError):
            audit.audit_snapshot([{"number": 0, "body": ""}], [])

    def test_git_hub_snapshot_shapes(self):
        self.assertEqual(audit.rows({"issues": [{"number": 2}]}, "issues"), [{"number": 2}])
        with self.assertRaises(ValueError):
            audit.rows({"issues": "not a list"}, "issues")

    def test_rust_only_results_keep_client_not_run(self):
        names = {id_: f"protocol5_e2e::{id_}" for id_ in CASES}
        log = "test protocol5_e2e::42-01 ... ok\ntest protocol5_e2e::42-02 ... FAILED\n"
        result = audit.compare_e2e(log, names, SHA)
        self.assertEqual(result["cases"][0]["rust_status"], "PASS")
        self.assertEqual(result["cases"][0]["client_status"], "NOT_RUN")
        self.assertEqual(result["cases"][0]["combined"], "NOT_RUN")
        self.assertEqual(result["cases"][1]["combined"], "FAIL")
        self.assertFalse(result["all_pass"])

    def test_mismatch_or_duplicate_fails_closed(self):
        names = {id_: f"case::{id_}" for id_ in CASES}
        with self.assertRaisesRegex(ValueError, "duplicate"):
            audit.compare_e2e("test case::42-01 ... ok\ntest case::42-01 ... ok\n", names, SHA)
        with self.assertRaisesRegex(ValueError, "mapping"):
            audit.compare_e2e("", {"42-01": "foo"}, SHA)
        with self.assertRaisesRegex(ValueError, "different commits"):
            audit.compare_e2e("", names, SHA, {"server_commit": "b" * 40}, Path("."))

    def test_verified_e42_pass_keeps_real_status_and_sha(self):
        names = {id_: f"case::{id_}" for id_ in CASES}
        report = new_report(SHA)
        report["client"].update(launcher="offline launcher", os_java_runtime="OpenJDK 8")
        report.update(executed_at="2026-11-16T12:00:00+09:00", connected_clients_peak=2,
                      server_launch_arguments_redacted="--world temporary")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            # NOT_RUN is valid client evidence and must never be elevated to PASS.
            result = audit.compare_e2e("test case::42-01 ... ok\n", names, SHA, report, root)
            self.assertEqual(result["cases"][0]["combined"], "NOT_RUN")
            self.assertEqual(result["cases"][0]["client_source_status"], "NOT_RUN")
            # The real-client validator rejects a claimed PASS without sealed evidence.
            report["cases"]["42-01"]["status"] = "PASS"
            with self.assertRaisesRegex(ValueError, "unverified"):
                audit.compare_e2e("test case::42-01 ... ok\n", names, SHA, report, root)


if __name__ == "__main__":
    unittest.main()

"""Fail-closed E42 real client acceptance report tests (no game executable)."""
import copy
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


spec = importlib.util.spec_from_file_location("e42_acceptance", Path(__file__).with_name("e42_acceptance.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
SHA = "a" * 40


class AcceptanceTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.report = module.new_report(SHA)
        self.report["client"]["launcher"] = "official launcher"
        self.report["client"]["os_java_runtime"] = "Windows / Java 8"
        self.report["executed_at"] = "2026-10-23T20:00:00+09:00"
        self.report["connected_clients_peak"] = 2
        self.report["server_launch_arguments_redacted"] = "--bind 127.0.0.1:25565"
        (self.root / "evidence").mkdir()
        for case, row in self.report["cases"].items():
            (self.root / "evidence" / f"{case}.log").write_text(
                f"{case}: redacted observed server/client events", encoding="utf-8"
            )
            row["status"] = "PASS"
            row["observation"] = "Observed on the unmodified client; manually confirmed"
            row["evidence"] = [f"evidence/{case}.log"]
        self.report = module.seal_report(self.report, self.root)

    def validate(self, **kwargs):
        return module.validate(self.report, self.root, require_pass=kwargs.get("require_pass", True))

    def test_pass_requires_real_evidence_and_all_nine_cases(self):
        self.assertEqual(self.validate(), [])
        self.assertEqual(len(self.report["cases"]), 9)

    def test_not_run_cannot_be_certified(self):
        self.report["cases"]["42-03"]["status"] = "NOT_RUN"
        self.assertTrue(any("42-03" in p for p in self.validate()))
        # Valid structure alone is not a test pass.
        self.assertEqual(self.validate(require_pass=False), [])

    def test_modded_client_and_one_player_fail_closed(self):
        self.report["client"]["unmodified"] = False
        self.report["connected_clients_peak"] = 1
        issues = self.validate()
        self.assertTrue(any("unmodified" in p for p in issues))
        self.assertTrue(any("42-05" in p for p in issues))
        self.assertTrue(any("42-09" in p for p in issues))

    def test_symlink_and_path_escape_rejected(self):
        row = self.report["cases"]["42-01"]
        row["evidence"] = [{"path": "../outside.log", "sha256": "a" * 64}]
        self.assertTrue(any("unsafe" in p for p in self.validate()))
        row["evidence"] = [{"path": "missing.png", "sha256": "a" * 64}]
        self.assertTrue(any("42-01" in p for p in self.validate()))

    def test_empty_log_and_missing_observation_fail(self):
        (self.root / "evidence" / "42-02.log").write_text("", encoding="utf-8")
        self.report["cases"]["42-02"]["observation"] = ""
        issues = self.validate()
        self.assertTrue(any("empty" in p for p in issues))
        self.assertTrue(any("42-02" in p for p in issues))

    def test_evidence_tampering_fails_even_when_nonempty(self):
        (self.root / "evidence" / "42-03.log").write_text(
            "modified after sealing", encoding="utf-8"
        )
        self.assertTrue(any("SHA-256 mismatch" in p for p in self.validate()))

    def test_shared_log_cannot_certify_nine_distinct_scenarios(self):
        shared = self.report["cases"]["42-01"]["evidence"][0].copy()
        for row in self.report["cases"].values():
            row["evidence"] = [shared.copy()]
        self.assertTrue(any("case-specific artifact" in p for p in self.validate()))

    def test_legacy_reports_can_be_inspected_but_cannot_certify_pass(self):
        self.report["schema_version"] = 1
        for case, row in self.report["cases"].items():
            row["evidence"] = [f"evidence/{case}.log"]
        self.assertEqual(self.validate(require_pass=False), [])
        self.assertTrue(any("schema v2" in p for p in self.validate()))

    def test_seal_is_non_mutating_and_hashes_each_artifact(self):
        original = module.new_report(SHA)
        original["cases"]["42-01"] = {
            "status": "PASS", "observation": "manual observation",
            "evidence": ["evidence/42-01.log"],
        }
        sealed = module.seal_report(original, self.root)
        self.assertEqual(original["cases"]["42-01"]["evidence"], ["evidence/42-01.log"])
        self.assertEqual(len(sealed["cases"]["42-01"]["evidence"][0]["sha256"]), 64)

    def test_new_reports_cannot_pass_unexecuted(self):
        empty = module.new_report(SHA)
        self.assertEqual(set(empty["cases"]), set(module.CASES))
        self.assertTrue(all(v["status"] == "NOT_RUN" for v in empty["cases"].values()))
        self.assertTrue(module.validate(empty, self.root, True))


if __name__ == "__main__":
    unittest.main()

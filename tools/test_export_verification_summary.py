"""Standalone tests for the offline verification-summary export."""
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
import sys
from io import StringIO

spec = importlib.util.spec_from_file_location(
    "export_verification_summary", Path(__file__).with_name("export_verification_summary.py")
)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ExportSummaryTests(unittest.TestCase):
    def fixture(self):
        return {
            "schema_version": 1,
            "commit": "a" * 40,
            "host_os": "windows",
            "host_arch": "x86_64",
            "checks": [{
                "name": "Rust unit/integration/Protocol 5 TCP",
                "status": "FAIL", "exit_code": 101,
                "command": "cargo test C:/Users/secret PRIVATE_TOKEN=password",
                "note": "password SECRET_DO_NOT_COPY",
                "log": "C:/Users/secret/private.log",
                "failed_tests": ["world::test_case", "../private/file"],
                "seed": "deadbeef-11",
            }],
        }

    def test_allowlist_keeps_reproduction_but_excludes_private_data(self):
        exported = module.export_report(self.fixture())
        serialized = json.dumps(exported)
        for secret in ["C:/Users", "PRIVATE_TOKEN", "password", "private.log", "../private/file"]:
            self.assertNotIn(secret, serialized)
        check = exported["checks"][0]
        self.assertEqual(check["failed_tests"], ["world::test_case"])
        self.assertEqual(check["test_seed"], "deadbeef-11")
        self.assertIn("cargo test --workspace", check["reproduce"])

    def test_invalid_status_and_duplicate_checks_are_rejected(self):
        data = self.fixture()
        data["checks"][0]["status"] = "SUCCESS"
        with self.assertRaises(ValueError):
            module.export_report(data)
        data["checks"][0]["status"] = "FAIL"
        data["checks"].append(dict(data["checks"][0]))
        with self.assertRaises(ValueError):
            module.export_report(data)

    def test_rejects_secret_like_seed_and_status_mismatch(self):
        data = self.fixture()
        data["checks"][0]["seed"] = "my_password_dont_share"
        self.assertIsNone(module.export_report(data)["checks"][0]["test_seed"])
        data["checks"][0].update(status="PASS", exit_code=101)
        with self.assertRaises(ValueError):
            module.export_report(data)

    def test_creates_exclusive_summary_and_preserves_source(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source, dest = root / "report.json", root / "safe.json"
            original = json.dumps(self.fixture())
            source.write_text(original, encoding="utf-8")
            module.write_export(source, dest)
            self.assertEqual(source.read_text(encoding="utf-8"), original)
            self.assertEqual(json.loads(dest.read_text())["commit"], "a" * 40)
            with self.assertRaises(FileExistsError):
                module.write_export(source, dest)
            with self.assertRaises(ValueError):
                module.write_export(source, source)

    def test_rejects_bad_report(self):
        with self.assertRaises(ValueError):
            module.export_report({"schema_version": 2, "checks": []})


    def test_not_run_with_exit_code_and_fail_with_zero_are_rejected(self):
        data = self.fixture()
        data["checks"][0].update(status="NOT_RUN", exit_code=0)
        with self.assertRaises(ValueError):
            module.export_report(data)
        data["checks"][0].update(status="FAIL", exit_code=0)
        with self.assertRaises(ValueError):
            module.export_report(data)
        # A process terminated by a signal may have no conventional exit code.
        data["checks"][0].update(status="FAIL", exit_code=None)
        self.assertEqual(module.export_report(data)["checks"][0]["status"], "FAIL")

    def test_cli_failure_never_echoes_sensitive_source_path(self):
        with tempfile.TemporaryDirectory(prefix="sensitive-private-archive-") as tmp:
            missing = Path(tmp) / "private-identifiable-report.json"
            output = Path(tmp) / "safe.json"
            stderr = StringIO()
            with patch.object(sys, "argv", ["export_verification_summary.py", str(missing), "--output", str(output)]):
                with patch.object(sys, "stderr", stderr):
                    with self.assertRaises(SystemExit) as result:
                        module.main()
            self.assertEqual(result.exception.code, 2)
            self.assertNotIn(str(missing), stderr.getvalue())
            self.assertNotIn("sensitive-private-archive", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()

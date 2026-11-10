"""Regression tests for strict automated/client verification gate split."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("check_verification_gates", Path(__file__).with_name("check_verification_gates.py"))
gates = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gates)


class GateTests(unittest.TestCase):
    def fixture(self, client="NOT_RUN"):
        rows = [{"name": n, "status": "PASS", "exit_code": 0} for n in gates.AUTO]
        rows.append({"name": gates.CLIENT, "status": client,
                     "exit_code": 0 if client == "PASS" else (101 if client == "FAIL" else None)})
        return {"schema_version": 1, "commit": "a" * 40, "checks": rows}

    def test_automated_pass_does_not_make_client_pass(self):
        result = gates.evaluate(self.fixture())
        self.assertEqual((result["automated"], result["client_e42"], result["all"]),
                         ("PASS", "NOT_RUN", "NOT_RUN"))
        self.assertEqual(gates.evaluate(self.fixture("PASS"))["all"], "PASS")

    def test_failed_check_is_not_hidden_by_other_passes(self):
        data = self.fixture("PASS")
        data["checks"][0].update(status="FAIL", exit_code=101)
        result = gates.evaluate(data)
        self.assertEqual(result["automated"], "FAIL")
        self.assertEqual(result["all"], "FAIL")
        self.assertEqual(result["client_e42"], "PASS")

    def test_missing_check_and_client_are_not_run(self):
        data = self.fixture("PASS")
        data["checks"] = data["checks"][1:-1]
        result = gates.evaluate(data)
        self.assertEqual(result["automated"], "NOT_RUN")
        self.assertEqual(result["client_e42"], "NOT_RUN")
        self.assertEqual(len(result["missing_checks"]), 2)

    def test_invalid_results_fail_closed(self):
        for mutation in ("duplicate", "unknown", "badpass", "badsha", "badnotrun"):
            data = self.fixture("PASS")
            if mutation == "duplicate":
                data["checks"].append(dict(data["checks"][0]))
            if mutation == "unknown":
                data["checks"].append({"name": "bypass_quality", "status": "PASS", "exit_code": 0})
            if mutation == "badpass":
                data["checks"][0]["exit_code"] = 101
            if mutation == "badsha":
                data["commit"] = "unknown"
            if mutation == "badnotrun":
                data["checks"][0].update(status="NOT_RUN", exit_code=0)
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                gates.evaluate(data)


if __name__ == "__main__":
    unittest.main()

"""Self-contained unit tests for isolated cargo benchmark verification."""
import importlib.util
import json
import os
from pathlib import Path
import platform
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("benchmark_cargo_builds", Path(__file__).with_name("benchmark_cargo_builds.py"))
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)


class BenchmarkTests(unittest.TestCase):
    def sample(self):
        return {
            "schema_version": 1, "status": "PASS", "system": "Windows",
            "machine": "AMD64", "cargo_version": "cargo 1.90.0",
            "rustc_version": "rustc 1.90.0", "profile": "release",
            "binaries": list(bench.BINARIES), "cold_elapsed_ms": 1000,
            "warm_elapsed_ms": 100,
        }

    def test_baseline_ratios_and_regression(self):
        before = self.sample()
        now = dict(before, cold_elapsed_ms=1200, warm_elapsed_ms=130)
        result = bench.compare_baseline(now, before, 1.25)
        self.assertEqual(result["status"], "FAIL")
        self.assertEqual(result["warm_ratio"], 1.3)
        now["warm_elapsed_ms"] = 120
        self.assertEqual(bench.compare_baseline(now, before, 1.25)["status"], "PASS")

    def test_incomparable_hosts_or_toolchains_are_not_pass(self):
        before = self.sample()
        for key, value in [("system", "Linux"), ("cargo_version", "cargo 1.91.0"),
                           ("binaries", ["vanilla-server"]), ("cold_elapsed_ms", -2)]:
            candidate = dict(before, **{key: value})
            self.assertEqual(bench.compare_baseline(candidate, before, 1.25)["status"], "INCOMPARABLE")
        with self.assertRaises(ValueError):
            bench.compare_baseline(before, before, 0.5)

    def test_fingerprint_and_exclusive_output(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            path = bench.executable_path(root, "vanilla-server", platform.system())
            path.parent.mkdir()
            path.write_bytes(b"binary-one")
            (path.parent / ("legacy-status-server" + (".exe" if platform.system() == "Windows" else ""))).write_bytes(b"binary-two")
            result = bench.fingerprints(root, platform.system())
            self.assertEqual(result["vanilla-server"]["bytes"], 10)
            self.assertEqual(len(result["legacy-status-server"]["sha256"]), 64)
            path.unlink()
            with self.assertRaises(ValueError):
                bench.fingerprints(root, platform.system())

    @unittest.skipIf(os.name == "nt", "Fake cargo script uses POSIX executable shebang")
    def test_stub_cargo_creates_a_new_target_without_deleting_previous(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            stub = root / "fake-cargo"
            stub.write_text(f'''#!{sys.executable}
import pathlib, sys
if "--version" in sys.argv:
 print("cargo 1.90.0"); raise SystemExit(0)
args = sys.argv
assert args[1:3] == ["build", "--release"]
folder = pathlib.Path(args[args.index("--target-dir") + 1]) / "release"
folder.mkdir(parents=True, exist_ok=True)
for name in ("vanilla-server", "legacy-status-server"):
 (folder / name).write_bytes(b"stable-binary")
print("build ok")
''', encoding="utf-8")
            stub.chmod(0o755)
            output = root / "only-new-output"
            first = bench.run(root, output, cargo=str(stub), rustc="missing-rustc")
            self.assertEqual(first["status"], "PASS")
            self.assertTrue(first["binary_hashes_unchanged"])
            self.assertTrue((output / "cold.log").is_file())
            self.assertTrue((output / "warm.log").is_file())
            self.assertTrue((output / "report.json").is_file())
            self.assertNotIn(str(root), (output / "report.json").read_text())
            with self.assertRaises(FileExistsError):
                bench.run(root, output, cargo=str(stub))
            self.assertEqual(json.loads((output / "report.json").read_text())["status"], "PASS")


if __name__ == "__main__":
    unittest.main()

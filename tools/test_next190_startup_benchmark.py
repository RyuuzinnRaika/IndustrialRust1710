"""Deterministic NEXT-190 harness checks: no server, sockets, or Cargo."""
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import next190_startup_benchmark as benchmark


class FakeChild:
    def __init__(self, *, stdout=None, wait_error=False):
        self.stdout = stdout or io.BytesIO(
            b"IndustrialRust1710: listening on 127.0.0.1:12345 (synthetic)\n")
        self.stdin = io.BytesIO()
        self.code = None
        self.wait_error = wait_error
        self.killed = False

    def poll(self):
        return self.code

    def kill(self):
        self.killed = True

    def wait(self, timeout=None):
        if self.wait_error:
            raise subprocess.TimeoutExpired("synthetic child", timeout)
        self.code = 0
        return self.code


class FailingClose(io.BytesIO):
    def close(self):
        super().close()
        raise OSError("synthetic close failure")


class FailingSecondLogWrite:
    def __init__(self, real):
        self.real = real
        self.writes = 0

    def write(self, data):
        self.writes += 1
        if self.writes == 2:
            raise OSError("synthetic reader log failure")
        return self.real.write(data)

    def flush(self):
        self.real.flush()

    def close(self):
        self.real.close()


class FailingLogFlush(FailingSecondLogWrite):
    def write(self, data):
        return self.real.write(data)

    def flush(self):
        raise OSError("synthetic reader flush failure")


def fake_join(_port, _name, result):
    result["initial_join_ns"] = result["spawn_before_ns"] + 1_000_000


class StartupHarnessTests(unittest.TestCase):
    def run_measure(self, child, directory, **kwargs):
        with mock.patch.object(benchmark.subprocess, "Popen", return_value=child), \
                mock.patch.object(benchmark, "join_measurement", side_effect=fake_join):
            return benchmark.measure(Path("synthetic-server"), directory, "cold", "ColdProbe", **kwargs)

    def test_close_error_after_graceful_exit_fails(self):
        stdout = FailingClose(b"IndustrialRust1710: listening on 127.0.0.1:12345 (synthetic)\n")
        with tempfile.TemporaryDirectory() as root:
            directory = Path(root) / "run"
            result = self.run_measure(FakeChild(stdout=stdout), directory)
            self.assertEqual(result["status"], "failed")
            self.assertTrue(any("synthetic close failure" in error for error in result["errors"]))
            self.assertEqual(json.loads((directory / "result.json").read_text()), result)

    def test_reader_log_failure_after_listening_fails(self):
        child = FakeChild(stdout=io.BytesIO(
            b"IndustrialRust1710: listening on 127.0.0.1:12345 (synthetic)\nextra\n"))
        original_open = Path.open

        def fake_open(path, *args, **kwargs):
            real = original_open(path, *args, **kwargs)
            return FailingSecondLogWrite(real) if path.name == "stdout.log" else real

        with tempfile.TemporaryDirectory() as root, mock.patch.object(Path, "open", fake_open):
            directory = Path(root) / "run"
            result = self.run_measure(child, directory)
            self.assertEqual(result["status"], "failed")
            self.assertTrue(any("synthetic reader log failure" in error for error in result["errors"]))
            self.assertEqual(json.loads((directory / "result.json").read_text()), result)

    def test_failed_kill_wait_is_recorded_as_forced(self):
        child = FakeChild(wait_error=True)
        with tempfile.TemporaryDirectory() as root, \
                mock.patch.object(benchmark.subprocess, "Popen", return_value=child), \
                mock.patch.object(benchmark, "join_measurement", side_effect=RuntimeError("synthetic join failure")):
            result = benchmark.measure(Path("synthetic-server"), Path(root) / "run", "cold", "ColdProbe")
            self.assertEqual(result["status"], "failed")
            self.assertTrue(result["forced_termination_attempted"])
            self.assertTrue(child.killed)
            self.assertTrue(any("child cleanup" in error for error in result["errors"]))

    def test_reader_flush_failure_is_not_a_pass(self):
        original_open = Path.open

        def fake_open(path, *args, **kwargs):
            real = original_open(path, *args, **kwargs)
            return FailingLogFlush(real) if path.name == "stdout.log" else real

        with tempfile.TemporaryDirectory() as root, mock.patch.object(Path, "open", fake_open):
            result = self.run_measure(FakeChild(), Path(root) / "run")
            self.assertEqual(result["status"], "failed")
            self.assertTrue(any("synthetic reader flush failure" in error for error in result["errors"]))

    def test_reader_still_alive_after_join_is_not_a_pass(self):
        readers = []

        class StuckReader:
            def __init__(self, *, target, args, daemon):
                self.events = args[2]
                self.log = args[1]
                readers.append(self)

            def start(self):
                self.events.put(("listening", 1, 12345))

            def join(self, timeout):
                pass

            def is_alive(self):
                return True

        with tempfile.TemporaryDirectory() as root, \
                mock.patch.object(benchmark.threading, "Thread", StuckReader):
            child = FakeChild()
            result = self.run_measure(child, Path(root) / "run")
            self.assertEqual(result["status"], "failed")
            self.assertTrue(any("stdout reader still running" in error for error in result["errors"]))
            # The fake thread never held the streams; release them explicitly.
            child.stdout.close()
            readers[0].log.close()

    def test_incomplete_walk_fails_closed(self):
        def failed_walk(_root, *, followlinks, onerror):
            onerror(PermissionError("synthetic walk failure"))
            return iter(())

        with mock.patch.object(benchmark.os, "walk", side_effect=failed_walk):
            with self.assertRaisesRegex(PermissionError, "synthetic walk failure"):
                benchmark.manifest_world(Path("synthetic"))

    def test_warm_result_and_manifest_include_same_copy_hashes(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            server = root / "fake-server"
            server.write_bytes(b"synthetic binary; never executed")
            output = root / "output"
            argv = ["benchmark", "--server", str(server), "--output", str(output),
                    "--revision", "synthetic", "--pairs", "1"]
            with mock.patch.object(sys, "argv", argv), \
                    mock.patch.object(benchmark.platform, "platform", return_value="synthetic"), \
                    mock.patch.object(benchmark.platform, "processor", return_value="synthetic"), \
                    mock.patch.object(benchmark.subprocess, "Popen", side_effect=lambda *a, **kw: FakeChild()), \
                    mock.patch.object(benchmark, "join_measurement", side_effect=fake_join):
                self.assertEqual(benchmark.main(), 0)
            report = json.loads((output / "manifest.json").read_text())
            warm = json.loads((output / "pair-00-warm" / "result.json").read_text())
            self.assertEqual(warm, report["runs"][1])
            self.assertIn("whitelist.json", warm["copied_fixture_sha256"])

    def test_copy_failure_marks_warm_failed(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            server = root / "fake-server"
            server.write_bytes(b"synthetic binary; never executed")
            output = root / "output"
            argv = ["benchmark", "--server", str(server), "--output", str(output),
                    "--revision", "synthetic", "--pairs", "1"]
            with mock.patch.object(sys, "argv", argv), \
                    mock.patch.object(benchmark.platform, "platform", return_value="synthetic"), \
                    mock.patch.object(benchmark.platform, "processor", return_value="synthetic"), \
                    mock.patch.object(benchmark.subprocess, "Popen", return_value=FakeChild()), \
                    mock.patch.object(benchmark, "join_measurement", side_effect=fake_join), \
                    mock.patch.object(benchmark, "copy_fixture", side_effect=PermissionError("synthetic copy failure")):
                self.assertEqual(benchmark.main(), 1)
            report = json.loads((output / "manifest.json").read_text())
            self.assertEqual(report["runs"][1]["status"], "failed")
            self.assertIn("synthetic copy failure", report["runs"][1]["errors"][0])

    def test_setup_failure_preserves_all_planned_not_run_rows(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            server = root / "fake-server"
            server.write_bytes(b"synthetic binary; never executed")
            output = root / "output"
            argv = ["benchmark", "--server", str(server), "--output", str(output),
                    "--revision", "synthetic", "--pairs", "2"]
            with mock.patch.object(sys, "argv", argv), \
                    mock.patch.object(benchmark, "sha256", side_effect=PermissionError("synthetic hash failure")):
                self.assertEqual(benchmark.main(), 1)
            report = json.loads((output / "manifest.json").read_text())
            self.assertIn("synthetic hash failure", report["harness_error"])
            self.assertEqual([run["status"] for run in report["runs"]], ["not_run"] * 4)


if __name__ == "__main__":
    unittest.main()

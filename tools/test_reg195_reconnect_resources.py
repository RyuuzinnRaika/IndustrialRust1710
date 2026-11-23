"""REG-195 verifier unit tests; do not launch or mutate a real server."""
import json
import io
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest import TestCase
from unittest.mock import patch
from types import SimpleNamespace
from contextlib import redirect_stdout, redirect_stderr

import verify_reg195_reconnect_resources as audit


class TestReconnectResourcesVerifier(TestCase):
    def test_online_count_recognizes_new_command_response_only(self):
        self.assertEqual(audit.ONLINE.findall("Online (0): \nOnline (1): Load0\n"),
                         ["0", "1"])
        self.assertEqual(audit.ONLINE.findall("not online (0): \n"), [])

    def test_trends_preserve_null_vs_measured_zero(self):
        samples = [
            {"rss_bytes": None, "threads": None, "fd_count": None},
            {"rss_bytes": 100, "threads": 4, "fd_count": 0},
            {"rss_bytes": 180, "threads": 5, "fd_count": 2},
        ]
        self.assertIsNone(audit.trend(samples, "rss_bytes")["delta"])
        self.assertIsNone(audit.trend(samples, "fd_count")["initial"])
        self.assertEqual(audit.trend(samples, "fd_count")["final"], 2)
        self.assertEqual(audit.trend(samples, "threads")["peak"], 5)
        self.assertIsNone(audit.trend([{"threads": None}], "threads")["delta"])

    def test_query_does_not_reuse_stale_list_output(self):
        class Child:
            def __init__(self):
                self.stdin = self
                self.written = b""

            def poll(self):
                return None

            def write(self, data):
                self.written += data

            def flush(self):
                pass

        with TemporaryDirectory() as temporary:
            log = Path(temporary) / "server.log"
            log.write_text("Online (0):\n", encoding="utf-8")
            child = Child()
            with patch.object(audit.time, "monotonic", side_effect=[0, 6]):
                with self.assertRaisesRegex(TimeoutError, "fresh"):
                    audit.query_online_count(child, log)
            self.assertEqual(child.written, b"/list\n")

    def test_manifest_explicitly_preserves_incomplete_state(self):
        with TemporaryDirectory() as temporary:
            directory = Path(temporary)
            audit.save_report(directory, {"completed_sessions": 0, "verified": False})
            result = json.loads((directory / "manifest.json").read_text())
            self.assertEqual(result["completed_sessions"], 0)
            self.assertFalse(result["verified"])
            with self.assertRaises(FileExistsError):
                audit.new_output_directory(directory)

    def run_fake_lifecycle(self, sessions, snapshots, threshold=None, expected_error=None,
                           measure_only=None, login_failure=False, kill_failure=False,
                           final_manifest_failure=False):
        class Child:
            pid = 123
            killed = False

            def poll(self):
                return 0 if self.killed else None

            def kill(self):
                if kill_failure:
                    raise OSError("synthetic cleanup failure")
                self.killed = True

            def wait(self, timeout):
                return 0

        class Client:
            def close(self):
                pass

        with TemporaryDirectory() as temporary:
            output = Path(temporary) / "fresh"
            kwargs = {} if measure_only is None else {"measure_only": measure_only}
            original_save = audit.save_report

            def save(path, report):
                if final_manifest_failure and "finished_utc" in report:
                    raise OSError("synthetic manifest failure")
                original_save(path, report)

            def login(*_):
                if login_failure:
                    raise RuntimeError("synthetic primary login failure")
                return Client()

            with patch.object(audit, "reserve_loopback_port", return_value=12345), \
                 patch.object(audit, "wait_until_listening"), \
                 patch.object(audit.platform, "platform", return_value="synthetic"), \
                 patch.object(audit, "full_login", side_effect=login), \
                 patch.object(audit, "query_online_count", return_value=1), \
                 patch.object(audit, "await_no_players"), \
                 patch.object(audit, "shutdown"), \
                 patch.object(audit, "proc_snapshot", side_effect=snapshots), \
                 patch.object(audit, "save_report", side_effect=save), \
                 patch.object(audit.time, "sleep"), \
                 patch.object(audit.subprocess, "Popen", return_value=Child()), \
                 patch.object(audit.subprocess, "run", return_value=SimpleNamespace(returncode=0)):
                if expected_error is None:
                    report = audit.run(Path("synthetic-server"), output, sessions, 3.2, threshold, **kwargs)
                    self.assertEqual(json.loads((output / "manifest.json").read_text()), report)
                else:
                    with self.assertRaisesRegex(*expected_error):
                        audit.run(Path("synthetic-server"), output, sessions, 3.2, threshold, **kwargs)
                    manifest = output / "manifest.json"
                    if manifest.exists():
                        report = json.loads(manifest.read_text())
                    else:
                        self.assertTrue(final_manifest_failure)
                        report = None
            return report

    def test_smoke_lifecycle_does_not_certify_full_audit(self):
        measured = {"rss_bytes": 100, "rss_peak_bytes": 120, "threads": 4, "fd_count": 3}
        report = self.run_fake_lifecycle(1, [measured, measured])
        self.assertFalse(report["verified"])
        self.assertTrue(report["lifecycle_verified"])
        self.assertTrue(report["metrics_complete"])
        self.assertFalse(report["full_1000_requested"])
        self.assertFalse(report["measurement_only_requested"])
        self.assertEqual(report["acceptance_status"], "NOT_RUN")

    def test_full_lifecycle_with_missing_counters_is_incomplete(self):
        missing = {"rss_bytes": None, "rss_peak_bytes": None, "threads": None, "fd_count": None}
        report = self.run_fake_lifecycle(1000, [missing] * 41)
        self.assertFalse(report["verified"])
        self.assertTrue(report["lifecycle_verified"])
        self.assertFalse(report["metrics_complete"])
        self.assertIsNone(report["rss"]["initial"])
        self.assertIsNone(report["rss"]["delta"])

    def test_full_lifecycle_requires_all_scheduled_samples(self):
        measured = {"rss_bytes": 100, "rss_peak_bytes": 120, "threads": 4, "fd_count": 3}
        incomplete = dict(measured, fd_count=None)
        snapshots = [measured] * 41
        snapshots[20] = incomplete
        report = self.run_fake_lifecycle(1000, snapshots)
        self.assertFalse(report["metrics_complete"])
        self.assertFalse(report["verified"])
        snapshots = [measured] * 40 + [incomplete]
        report = self.run_fake_lifecycle(1000, snapshots)
        self.assertIsNone(report["fd"]["final"])
        self.assertIsNone(report["fd"]["delta"])
        self.assertFalse(report["verified"])

    def test_full_lifecycle_and_complete_samples_are_reported_separately(self):
        measured = {"rss_bytes": 100, "rss_peak_bytes": 120, "threads": 4, "fd_count": 3}
        report = self.run_fake_lifecycle(1000, [measured] * 41)
        self.assertTrue(report["lifecycle_verified"])
        self.assertTrue(report["metrics_complete"])
        self.assertFalse(report["verified"])
        self.assertEqual(report["retained_sessions_status"], "NOT_MEASURED")
        self.assertEqual(report["acceptance_status"], "NOT_RUN")
        self.assertEqual(report["rss_limit_status"], "NOT_REQUESTED")
        self.assertEqual(report["rss"]["delta"], 0)
        report = self.run_fake_lifecycle(1000, [measured] * 41, threshold=0)
        self.assertEqual(report["rss_limit_status"], "PASS")
        self.assertEqual(report["max_rss_growth_mib"], 0)
        self.assertEqual(report["acceptance_status"], "NOT_RUN")
        self.assertFalse(report["verified"])
        report = self.run_fake_lifecycle(1, [measured, measured], measure_only=True)
        self.assertTrue(report["measurement_only_requested"])
        self.assertEqual(report["acceptance_status"], "NOT_RUN")
        self.assertFalse(report["verified"])

    def test_requested_rss_limit_distinguishes_unavailable_and_exceeded(self):
        missing = {"rss_bytes": None, "rss_peak_bytes": None, "threads": None, "fd_count": None}
        unavailable = self.run_fake_lifecycle(
            1, [missing, missing], threshold=0,
            expected_error=(audit.ResourceMeasurementUnavailable, "unavailable"))
        self.assertEqual(unavailable["rss_limit_status"], "UNAVAILABLE")
        self.assertEqual(unavailable["acceptance_status"], "NOT_RUN")
        self.assertFalse(unavailable["verified"])

        first = {"rss_bytes": 100, "rss_peak_bytes": 120, "threads": 4, "fd_count": 3}
        last = dict(first, rss_bytes=101)
        exceeded = self.run_fake_lifecycle(
            1, [first, last], threshold=0,
            expected_error=(RuntimeError, "exceeded"))
        self.assertEqual(exceeded["rss_limit_status"], "FAIL")
        self.assertEqual(exceeded["acceptance_status"], "NOT_RUN")
        self.assertFalse(exceeded["verified"])

    def test_cleanup_failure_keeps_primary_error_and_saves_incomplete_manifest(self):
        measured = {"rss_bytes": 100, "rss_peak_bytes": 120, "threads": 4, "fd_count": 3}
        with redirect_stderr(io.StringIO()) as stderr:
            report = self.run_fake_lifecycle(
                1, [measured], login_failure=True, kill_failure=True,
                expected_error=(RuntimeError, "synthetic primary login failure"))
        self.assertIn("synthetic cleanup failure", stderr.getvalue())
        self.assertIn("synthetic cleanup failure", report["cleanup_errors"][0])
        self.assertFalse(report["lifecycle_verified"])
        self.assertFalse(report["verified"])

    def test_final_manifest_failure_is_not_reported_as_success(self):
        measured = {"rss_bytes": 100, "rss_peak_bytes": 120, "threads": 4, "fd_count": 3}
        with redirect_stderr(io.StringIO()) as stderr:
            report = self.run_fake_lifecycle(
                1, [measured, measured], final_manifest_failure=True,
                expected_error=(OSError, "synthetic manifest failure"))
        self.assertIn("final manifest write failed", stderr.getvalue())
        self.assertNotIn("finished_utc", report)
        self.assertFalse(report["lifecycle_verified"])
        self.assertFalse(report["verified"])

    def test_final_manifest_failure_does_not_hide_original_failure(self):
        measured = {"rss_bytes": 100, "rss_peak_bytes": 120, "threads": 4, "fd_count": 3}
        with redirect_stderr(io.StringIO()) as stderr:
            report = self.run_fake_lifecycle(
                1, [measured], login_failure=True, final_manifest_failure=True,
                expected_error=(RuntimeError, "synthetic primary login failure"))
        self.assertIsNone(report)
        self.assertIn("synthetic manifest failure", stderr.getvalue())

    def test_callers_active_exception_does_not_hide_manifest_failure(self):
        measured = {"rss_bytes": 100, "rss_peak_bytes": 120, "threads": 4, "fd_count": 3}
        try:
            raise ValueError("unrelated caller exception")
        except ValueError:
            with redirect_stderr(io.StringIO()) as stderr:
                report = self.run_fake_lifecycle(
                    1, [measured, measured], final_manifest_failure=True,
                    expected_error=(OSError, "synthetic manifest failure"))
        self.assertIn("synthetic manifest failure", stderr.getvalue())
        self.assertFalse(report["verified"])

    def test_callers_active_exception_does_not_hide_cleanup_failure(self):
        class Child:
            def poll(self):
                return None

            def kill(self):
                raise OSError("synthetic cleanup failure")

            def wait(self, timeout):
                return 0

        report = {"lifecycle_verified": True, "verified": False}
        try:
            raise ValueError("unrelated caller exception")
        except ValueError:
            with TemporaryDirectory() as temporary, redirect_stderr(io.StringIO()) as stderr:
                with self.assertRaisesRegex(RuntimeError, "cleanup failed"):
                    audit.finish_report(Path(temporary), report, None, Child(), None)
        self.assertIn("synthetic cleanup failure", stderr.getvalue())
        self.assertFalse(report["lifecycle_verified"])

    def test_cleanup_and_manifest_failures_are_both_visible(self):
        class Child:
            def poll(self):
                return None

            def kill(self):
                raise OSError("synthetic cleanup failure")

            def wait(self, timeout):
                return 0

        report = {"lifecycle_verified": True, "verified": False}
        stderr = io.StringIO()
        with TemporaryDirectory() as temporary, \
             patch.object(audit, "save_report", side_effect=OSError("synthetic manifest failure")), \
             redirect_stderr(stderr):
            with self.assertRaisesRegex(OSError, "synthetic manifest failure"):
                audit.finish_report(Path(temporary), report, None, Child(), None)
        self.assertIn("synthetic cleanup failure", stderr.getvalue())
        self.assertIn("synthetic manifest failure", stderr.getvalue())
        self.assertFalse(report["lifecycle_verified"])
        self.assertFalse(report["verified"])

    def test_cli_distinguishes_smoke_missing_metrics_and_full_measurement(self):
        with TemporaryDirectory() as temporary:
            server = Path(temporary) / "synthetic-server"
            server.write_bytes(b"synthetic placeholder; never executed")
            argv = ["verify", "--server", str(server), "--output", str(Path(temporary) / "out")]
            scenarios = [
                ({"completed_sessions": 1, "full_1000_requested": False, "metrics_complete": True}, [], 0, "SMOKE:"),
                ({"completed_sessions": 1000, "full_1000_requested": True, "metrics_complete": False}, [], 2, "INCOMPLETE:"),
                ({"completed_sessions": 1000, "full_1000_requested": True, "metrics_complete": False}, ["--measure-only"], 2, "INCOMPLETE:"),
                ({"completed_sessions": 1000, "full_1000_requested": True, "metrics_complete": True}, [], 2, "MEASURED:"),
                ({"completed_sessions": 1000, "full_1000_requested": True, "metrics_complete": True}, ["--measure-only"], 0, "MEASURED:"),
            ]
            for report, flags, expected_exit, expected_prefix in scenarios:
                stdout, stderr = io.StringIO(), io.StringIO()
                with patch.object(audit.sys, "argv", argv + flags), patch.object(audit, "run", return_value=report) as run, \
                     redirect_stdout(stdout), redirect_stderr(stderr):
                    self.assertEqual(audit.main(), expected_exit)
                self.assertEqual(run.call_args.args[-1], bool(flags))
                self.assertIn(expected_prefix, stdout.getvalue() + stderr.getvalue())

            for error, expected_exit, expected_prefix in [
                (audit.ResourceMeasurementUnavailable("unavailable"), 2, "INCOMPLETE:"),
                (RuntimeError("RSS growth exceeded"), 1, "FAIL/INCOMPLETE:"),
            ]:
                for sessions in ("1", "1000"):
                    for flags in ([], ["--measure-only"]):
                        stdout, stderr = io.StringIO(), io.StringIO()
                        args = argv + ["--sessions", sessions, "--max-rss-growth-mib", "0"] + flags
                        with patch.object(audit.sys, "argv", args), patch.object(audit, "run", side_effect=error), \
                             redirect_stdout(stdout), redirect_stderr(stderr):
                            self.assertEqual(audit.main(), expected_exit)
                        self.assertIn(expected_prefix, stderr.getvalue())

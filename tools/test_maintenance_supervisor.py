"""Standard-library tests for the optional J92 maintenance supervisor."""
import importlib.util
import io
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "maintenance_supervisor", Path(__file__).with_name("maintenance_supervisor.py")
)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class MaintenanceTests(unittest.TestCase):
    def setUp(self):
        self.server = Path("/test/server")
        self.world = Path("/test/world/flat.irw")
        self.backups = Path("/test/backups")

    @patch.object(module.subprocess, "run")
    def test_verify_always_precedes_rotating_backup(self, run):
        module.maintenance(self.server, self.world, self.backups, 4)
        self.assertEqual(run.call_count, 2)
        self.assertEqual(
            run.call_args_list[0].args[0],
            [str(self.server), "--verify-world", str(self.world)],
        )
        self.assertEqual(
            run.call_args_list[1].args[0],
            [str(self.server), "--world", str(self.world),
             "--backup-rotate", str(self.backups), "4"],
        )
        self.assertTrue(all(c.kwargs["check"] for c in run.call_args_list))

    @patch.object(module.subprocess, "run")
    def test_bad_world_never_rotates_or_restarts(self, run):
        run.side_effect = subprocess.CalledProcessError(1, "invalid-world")
        with self.assertRaises(subprocess.CalledProcessError):
            module.maintenance(self.server, self.world, self.backups, 2)
        self.assertEqual(run.call_count, 1)

    def test_timeout_does_not_force_kill_and_never_backs_up(self):
        class NeverStops:
            stdin = io.BytesIO()
            def poll(self):
                return None
            def wait(self, timeout):
                raise subprocess.TimeoutExpired("server", timeout)
        runner = module.Supervisor(["server"], self.server, self.world,
                                   self.backups, 2, 3600, 5)
        child = NeverStops()
        with self.assertRaisesRegex(RuntimeError, "timed out"):
            runner._stop(child)
        self.assertEqual(child.stdin.getvalue(), b"/stop\n")

    def test_clean_shutdown_is_the_only_successful_stop(self):
        class Stops:
            stdin = io.BytesIO()
            def poll(self):
                return None
            def wait(self, timeout):
                return 0
        runner = module.Supervisor(["server"], self.server, self.world,
                                   self.backups, 2, 3600, 5)
        child = Stops()
        runner.child = child
        runner._stop(child)
        self.assertIsNone(runner.child)
        self.assertEqual(child.stdin.getvalue(), b"/stop\n")


if __name__ == "__main__":
    unittest.main()

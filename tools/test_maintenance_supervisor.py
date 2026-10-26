"""Standard-library tests for the optional J92 maintenance supervisor."""
import importlib.util
import io
from pathlib import Path
import subprocess
import tempfile
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

    @patch.object(module, "prune_verified_backups")
    @patch.object(module, "owned_generations",
                  side_effect=[[], [Path("/test/backups/backup-001")]])
    @patch.object(module, "backup_names", side_effect=[set(), {"backup-001"}])
    @patch.object(module.subprocess, "run")
    def test_verify_rotate_and_restore_before_restart(self, run, names, owned, prune):
        module.maintenance(self.server, self.world, self.backups, 4)
        self.assertEqual(run.call_count, 3)
        self.assertEqual(names.call_count, 2)
        self.assertEqual(
            run.call_args_list[0].args[0],
            [str(self.server), "--verify-world", str(self.world)],
        )
        self.assertEqual(
            run.call_args_list[1].args[0],
            [str(self.server), "--world", str(self.world),
             "--backup-rotate", str(self.backups), "4"],
        )
        self.assertEqual(run.call_args_list[2].args[0][-2:],
                         ["--backup", str(self.backups / "backup-001")])
        self.assertTrue(all(c.kwargs["check"] for c in run.call_args_list))
        prune.assert_called_once_with(
            self.backups, 4, self.backups / "backup-001")
        self.assertEqual(owned.call_count, 2)

    @patch.object(module.subprocess, "run")
    def test_bad_world_never_rotates_or_restarts(self, run):
        run.side_effect = subprocess.CalledProcessError(1, "invalid-world")
        with self.assertRaises(subprocess.CalledProcessError):
            module.maintenance(self.server, self.world, self.backups, 2)
        self.assertEqual(run.call_count, 1)

    @patch.object(module, "owned_generations", return_value=[])
    @patch.object(module, "backup_names", side_effect=[set(), set()])
    @patch.object(module.subprocess, "run")
    def test_missing_backup_refuses_to_restart(self, run, names, owned):
        with self.assertRaisesRegex(RuntimeError, "uniquely identify"):
            module.maintenance(self.server, self.world, self.backups, 1)
        self.assertEqual(run.call_count, 2)
        self.assertEqual(names.call_count, 2)
        owned.assert_called_once_with(self.backups)

    @patch.object(module, "prune_verified_backups")
    @patch.object(module, "owned_generations",
                  side_effect=[[], [Path("/test/backups/backup-001")]])
    @patch.object(module, "backup_names", side_effect=[set(), {"backup-001"}])
    @patch.object(module.subprocess, "run")
    def test_restore_drill_failure_blocks_restart(self, run, names, owned, prune):
        run.side_effect = [None, None, subprocess.CalledProcessError(1, "restore")]
        with self.assertRaises(subprocess.CalledProcessError):
            module.maintenance(self.server, self.world, self.backups, 2)
        self.assertEqual(run.call_count, 3)
        self.assertEqual(owned.call_count, 2)
        prune.assert_not_called()

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




class RetentionSafetyTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="f55-generations-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.backups = self.root / "backups"
        self.backups.mkdir()
        self.server = self.root / "server"
        self.world = self.root / "world" / "flat.irw"

    def generation(self, name):
        folder = self.backups / name
        folder.mkdir()
        (folder / ".industrial1710-backup").write_bytes(module.BACKUP_MARKER)
        (folder / "flat.irw").write_bytes(b"existing-backup")
        return folder

    def test_restore_failure_preserves_all_earlier_generations(self):
        first = self.generation("backup-100-1")
        second = self.generation("backup-200-1")
        new = self.backups / "backup-300-1"
        foreign = self.backups / "backup-external"
        foreign.mkdir()
        calls = []

        def fake_run(argv, **kwargs):
            calls.append(argv)
            if len(calls) == 2:
                self.generation(new.name)
            elif len(calls) == 3:
                self.assertTrue(first.exists())
                self.assertTrue(second.exists())
                raise subprocess.CalledProcessError(1, argv)
        with patch.object(module.subprocess, "run", side_effect=fake_run):
            with self.assertRaises(subprocess.CalledProcessError):
                module.maintenance(self.server, self.world, self.backups, 1)
        # The native rotator was explicitly asked to preserve old generations
        # until a separately verified restore has passed.
        self.assertEqual(calls[1][-1], "3")
        self.assertTrue(first.exists())
        self.assertTrue(second.exists())
        self.assertTrue(new.exists())
        self.assertTrue(foreign.exists())

    def test_success_prunes_only_after_restore_and_ignores_external(self):
        first = self.generation("backup-100-1")
        second = self.generation("backup-200-1")
        new = self.backups / "backup-300-1"
        foreign = self.backups / "backup-external"
        foreign.mkdir()
        calls = []

        def fake_run(argv, **kwargs):
            calls.append(argv)
            if len(calls) == 2:
                self.generation(new.name)
            elif len(calls) == 3:
                # The restore drill must observe all earlier owned backups.
                self.assertTrue(first.exists())
                self.assertTrue(second.exists())
                self.assertTrue(new.exists())
        with patch.object(module.subprocess, "run", side_effect=fake_run):
            module.maintenance(self.server, self.world, self.backups, 1)
        self.assertEqual(calls[1][-1], "3")
        self.assertEqual(len(calls), 3)
        self.assertFalse(first.exists())
        self.assertFalse(second.exists())
        self.assertTrue(new.exists())
        self.assertTrue(foreign.exists())

    def test_clock_rollback_never_prunes_new_verified_generation(self):
        old = self.generation("backup-900-1")
        new = self.generation("backup-100-1")
        module.prune_verified_backups(self.backups, 1, new)
        self.assertTrue(new.exists())
        self.assertFalse(old.exists())

    def test_refuses_unmarked_generation_without_pruning(self):
        original = self.generation("backup-900-1")
        bogus = self.backups / "backup-901-1"
        bogus.mkdir()
        with self.assertRaisesRegex(RuntimeError, "disappeared"):
            module.prune_verified_backups(self.backups, 1, bogus)
        self.assertTrue(original.exists())

    def test_capacity_limit_never_deletes_existing_backups(self):
        generations = [self.generation(f"backup-{i:04}-1") for i in range(100)]
        with patch.object(module.subprocess, "run") as run:
            with self.assertRaisesRegex(RuntimeError, "100 backup generations"):
                module.maintenance(self.server, self.world, self.backups, 2)
        self.assertEqual(run.call_count, 1)  # read-only verification only
        self.assertTrue(all(path.exists() for path in generations))

if __name__ == "__main__":
    unittest.main()

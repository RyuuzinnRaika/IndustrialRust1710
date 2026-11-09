"""REG-187 helper tests (do not run actual Rust processes)."""
import hashlib
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest import TestCase

import reg187_stop_restart_lock as race


class SessionLockRaceVerifierTests(TestCase):
    def test_busy_lock_is_not_a_generic_startup_failure(self):
        message = "Error: world session lock is busy at /tmp/world/session.lock"
        self.assertTrue(race.log_has_lock_busy(message))
        self.assertFalse(race.log_has_lock_busy("Address already in use"))
        self.assertFalse(race.log_has_lock_busy("World file corrupted"))
        self.assertFalse(race.log_has_lock_busy("session.lock is not writable"))

    def test_hash_is_stable_and_changes_for_new_generation(self):
        with TemporaryDirectory() as scratch:
            path = Path(scratch) / "world.irw"
            path.write_bytes(b"IRW" + b"A" * 100)
            before = race.file_hash(path)
            self.assertEqual(before, hashlib.sha256(path.read_bytes()).hexdigest())
            self.assertEqual(before, race.file_hash(path))
            path.write_bytes(b"IRW" + b"B" * 100)
            self.assertNotEqual(before, race.file_hash(path))

    def test_new_output_rejects_any_existing_directory(self):
        with TemporaryDirectory() as scratch:
            with self.assertRaises(FileExistsError):
                race.new_output_directory(Path(scratch))

"""Contract checks for the isolated TNT stress harness; no server process."""
import struct
import tempfile
import unittest
from pathlib import Path

from tnt_stress_benchmark import seed_irw, tnt_snapshot_count


class SeededTntTest(unittest.TestCase):
    def test_legacy_tnt1_profiles(self):
        with tempfile.TemporaryDirectory() as root:
            for count in (100, 500, 1000, 4096):
                target = Path(root) / f"tnt-{count}.irw"
                seed_irw(target, count)
                self.assertEqual(tnt_snapshot_count(target), ("TNT1", count))
                raw = target.read_bytes()
                self.assertEqual(len(raw), 8 + 6 + 20 * count)
                ids = {struct.unpack(">i", raw[14 + n * 20:18 + n * 20])[0]
                       for n in range(count)}
                self.assertEqual(len(ids), count)

    def test_tnt2_queue_and_fresh_file_guard(self):
        with tempfile.TemporaryDirectory() as root:
            target = Path(root) / "world.irw"
            seed_irw(target, 100, chain_queue=True)
            self.assertEqual(tnt_snapshot_count(target), ("TNT2", 100))
            with self.assertRaises(FileExistsError):
                seed_irw(target, 100)

    def test_tnt4_kinematic_trailer_count(self):
        with tempfile.TemporaryDirectory() as root:
            target = Path(root) / "world.irw"
            seed_irw(target, 100)
            old = target.read_bytes()
            expanded = bytearray(old[:8] + b"TNT4" + old[12:14])
            for i in range(100):
                expanded += old[14 + 20 * i:14 + 20 * (i + 1)]
                expanded += bytes(24)  # three positions and three velocities
            expanded += bytes(8)  # empty chain + blast queues
            target.write_bytes(expanded)
            self.assertEqual(tnt_snapshot_count(target), ("TNT4", 100))

    def test_corrupted_or_unsupported_trailer_fails_closed(self):
        with tempfile.TemporaryDirectory() as root:
            target = Path(root) / "world.irw"
            target.write_bytes(b"IRW1" + struct.pack(">I", 0) + b"BAD!")
            with self.assertRaises(ValueError):
                tnt_snapshot_count(target)
            target.write_bytes(b"IRW1" + struct.pack(">I", 0) +
                               b"TNT1" + struct.pack(">H", 4096))
            with self.assertRaises(ValueError):
                tnt_snapshot_count(target)


if __name__ == "__main__":
    unittest.main()

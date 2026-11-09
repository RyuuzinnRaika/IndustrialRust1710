"""REG-192 packet/measurement checks (no Rust server needed)."""
import struct
import unittest

import reg192_chunk_fairness as fairness


class ChunkFairnessUnitTests(unittest.TestCase):
    def test_protocol5_s21_load_and_unload_are_not_conflated(self):
        loaded = struct.pack(">iiBHHI", -8, 3, 1, 0x18, 0, 3) + b"xyz"
        self.assertEqual(fairness.parse_chunk(loaded), (-8, 3))
        unload = struct.pack(">iiBHHI", -8, 3, 1, 0, 0, 0)
        self.assertIsNone(fairness.parse_chunk(unload))
        with self.assertRaisesRegex(ValueError, "length"):
            fairness.parse_chunk(loaded[:-1])
        with self.assertRaisesRegex(ValueError, "bitmap"):
            fairness.parse_chunk(struct.pack(">iiBHHI", 1, 2, 1, 0, 0, 1) + b"a")

    def test_boundary_floor_works_on_negative_coordinates(self):
        self.assertEqual(fairness.chunk_coord(-0.001), -1)
        self.assertEqual(fairness.chunk_coord(-16.0), -1)
        self.assertEqual(fairness.chunk_coord(-16.001), -2)
        self.assertEqual(fairness.chunk_coord(15.999), 0)
        self.assertEqual(fairness.chunk_coord(16.0), 1)

    def test_only_new_columns_after_crossing_count(self):
        samples = [
            (1.0, (0, 0)),
            (3.2, (1, 0)),  # already loaded before crossing
            (4.5, (2, 0)),
            (4.6, (2, 0)),
            (4.7, (3, 0)),
        ]
        result = fairness.summarize(samples, {(0, 0), (1, 0)}, 4.0)
        self.assertEqual(result["new_chunks"], 2)
        self.assertEqual(result["new_packets"], 3)
        self.assertEqual(result["first_new_chunk_ms"], 500.0)
        self.assertEqual(fairness.summarize(samples, {(0, 0), (1, 0)}, 5)["new_chunks"], 0)

    def test_p95_uses_observed_distribution_not_mean(self):
        self.assertEqual(fairness.quantile95([1, 2, 3, 4, 100]), 100)
        self.assertEqual(fairness.quantile95(list(range(1, 101))), 95)
        with self.assertRaises(ValueError):
            fairness.quantile95([])


if __name__ == "__main__":
    unittest.main()

"""Unit checks for the opt-in E48/E50 synthetic TCP driver (no server required)."""
import importlib.util
import io
from pathlib import Path
import socket
import struct
import tempfile
import unittest


spec = importlib.util.spec_from_file_location(
    "e48_load_benchmark", Path(__file__).with_name("e48_load_benchmark.py")
)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class BenchmarkProtocolTests(unittest.TestCase):
    def test_varint_and_packet_wire_bytes(self):
        self.assertEqual(module.varint(0), b"\x00")
        self.assertEqual(module.varint(127), b"\x7f")
        self.assertEqual(module.varint(128), b"\x80\x01")
        self.assertEqual(module.frame(0, module.mc_string("A")),
                         b"\x03\x00\x01A")
        with self.assertRaises(ValueError):
            module.varint(-1)

    def test_frame_reading_handles_fragmentation_and_rejects_malformed(self):
        left, right = socket.socketpair()
        try:
            left.sendall(b"\x03\x00\x01A\x02\x03B")
            self.assertEqual(module.read_frame(right), (0, b"\x01A"))
            self.assertEqual(module.read_frame(right), (3, b"B"))
            left.sendall(b"\x00")  # no zero-length frames
            with self.assertRaises(ValueError):
                module.read_frame(right)
            left.sendall(b"\x80\x80\x80\x80\x10")  # overlong fifth byte
            with self.assertRaises(ValueError):
                module.read_frame(right)
            left.sendall(module.varint(module.MAX_FRAME + 1))
            with self.assertRaises(ValueError):
                module.read_frame(right)
        finally:
            left.close()
            right.close()

    def test_truncated_frame_fails_closed(self):
        left, right = socket.socketpair()
        try:
            left.sendall(b"\x05ab")
            left.close()
            with self.assertRaises(EOFError):
                module.read_frame(right)
        finally:
            left.close()
            right.close()


class TelemetryValidationTests(unittest.TestCase):
    def row(self, min_players=4, peak_players=4, processed=195, missed=5):
        return {
            "schema_version": 2,
            "players_min": min_players,
            "players_peak": peak_players,
            "window_seconds": 10.0,
            "processed_ticks": processed,
            "missed_ticks": missed,
            "mspt_samples": processed,
            "mspt_p99": 21.0,
            "process_rss_bytes": 1024,
        }

    def test_only_fixed_peer_load_windows_are_measured(self):
        rows = [self.row(1, 4), self.row(), self.row(processed=200, missed=0)]
        result = module.stable_summary(rows, 4)
        self.assertEqual(result["stable_windows"], 2)
        self.assertEqual(result["stable_seconds"], 20)
        self.assertEqual(result["tps_processed"], 19.75)
        self.assertAlmostEqual(result["tick_delivery_ratio"], 395 / 400)
        self.assertEqual(result["mspt_p99_worst_window"], 21.0)
        self.assertEqual(result["process_rss_peak_bytes"], 1024)

    def test_soak_trends_require_six_complete_stable_windows(self):
        rows = [self.row() for _ in range(6)]
        for index, row in enumerate(rows):
            row["process_rss_bytes"] = (10 + index) * 1024 * 1024
            row["processed_ticks"] = 200 if index < 3 else 180
        result = module.stable_summary(rows, 4)
        self.assertEqual(result["process_rss_growth_bytes"], 5 * 1024 * 1024)
        self.assertAlmostEqual(result["tps_first_half"], 20.0)
        self.assertAlmostEqual(result["tps_second_half"], 18.0)
        self.assertIsNone(module.stable_summary(rows[:5], 4)["process_rss_growth_bytes"])
        del rows[2]["process_rss_bytes"]
        self.assertIsNone(module.stable_summary(rows, 4)["process_rss_growth_bytes"])

    def test_no_stable_window_is_not_a_performance_pass(self):
        with self.assertRaisesRegex(ValueError, "no complete"):
            module.stable_summary([self.row(1, 4)], 4)
        with self.assertRaisesRegex(ValueError, "schema v2"):
            module.stable_summary([{"schema_version": 1}], 4)

    def test_bogus_stable_values_are_rejected(self):
        for bad in [
            {"window_seconds": float("nan")},
            {"processed_ticks": -1},
            {"mspt_samples": 0},
            {"mspt_p99": float("inf")},
        ]:
            row = self.row()
            row.update(bad)
            with self.assertRaises(ValueError):
                module.stable_summary([row], 4)

    def test_output_directory_cannot_be_reused(self):
        with tempfile.TemporaryDirectory() as root:
            new_path = Path(root) / "results"
            module.new_output_directory(new_path)
            with self.assertRaises(FileExistsError):
                module.new_output_directory(new_path)


if __name__ == "__main__":
    unittest.main()

"""REG-196 helper tests: no Rust process and no existing world are touched."""
import json
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest import TestCase
from unittest.mock import patch

import reg196_mixed_soak as soak


class TestMixedSoakVerifier(TestCase):
    def test_mining_packets_encode_legacy_c07_start_and_finish(self):
        class Client:
            packets = []

            def send(self, packet_id, body):
                self.packets.append((packet_id, body))

        client = Client()
        with patch.object(soak.time, "sleep"):
            soak.mine(client)
        self.assertEqual([packet[0] for packet in client.packets], [7, 7])
        self.assertEqual([len(packet[1]) for packet in client.packets], [11, 11])
        self.assertEqual(client.packets[0][1][0], 0)
        self.assertEqual(client.packets[1][1][0], 2)
        self.assertEqual(client.packets[0][1][1:], client.packets[1][1][1:])

    def test_telemetry_rejects_missing_or_invalid_windows(self):
        with TemporaryDirectory() as directory:
            path = Path(directory) / "metrics.jsonl"
            with self.assertRaisesRegex(RuntimeError, "missing"):
                soak.summarize_telemetry(path)
            path.write_text('{"schema_version":1,"window_seconds":10}\n')
            with self.assertRaisesRegex(RuntimeError, "no valid"):
                soak.summarize_telemetry(path)
            rows = [
                {"schema_version": 2, "window_seconds": 10,
                 "processed_ticks": 200, "missed_ticks": 0,
                 "mspt_p99": 6.0, "process_rss_bytes": 1000},
                {"schema_version": 2, "window_seconds": 10,
                 "processed_ticks": 198, "missed_ticks": 2,
                 "mspt_p99": 9.0, "process_rss_bytes": 1400},
            ]
            path.write_text("".join(json.dumps(r) + "\n" for r in rows))
            summary = soak.summarize_telemetry(path)
            self.assertEqual(summary["tps_processed"], 19.9)
            self.assertEqual(summary["missed_ticks"], 2)
            self.assertEqual(summary["rss_delta_first_last"], 400)

    def test_log_read_requires_a_fresh_success_marker(self):
        class Child:
            def poll(self):
                return None

        with TemporaryDirectory() as directory:
            path = Path(directory) / "server.log"
            path.write_text("Block set at 4 72 4\n")
            old_end = path.stat().st_size
            with patch.object(soak.time, "monotonic", side_effect=[0, 20]):
                with self.assertRaises(TimeoutError):
                    soak.wait_log(Child(), path, "Block set at 4 72 4", old_end)
            with path.open("a") as output:
                output.write("Block set at 4 72 4\n")
            soak.wait_log(Child(), path, "Block set at 4 72 4", old_end)

    def test_test_world_output_refuses_existing_directory(self):
        with TemporaryDirectory() as directory:
            with self.assertRaises(FileExistsError):
                soak.new_output_directory(Path(directory))

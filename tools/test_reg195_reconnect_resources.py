"""REG-195 verifier unit tests; do not launch or mutate a real server."""
import json
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest import TestCase
from unittest.mock import patch

import reg195_reconnect_resources as audit


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
        self.assertEqual(audit.trend(samples, "rss_bytes")["delta"], 80)
        self.assertEqual(audit.trend(samples, "fd_count")["initial"], 0)
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

"""Local-only status fixtures for the no-GitHub-Actions readiness probe."""

import contextlib
import json
import socket
import threading
import unittest

import check_server_ready as ready


@contextlib.contextmanager
def status_server(protocol=5, label="IndustrialRust1710 vanilla-only prototype", malformed=False, document=None):
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", 0))
    listener.listen(1)
    listener.settimeout(3)
    port = listener.getsockname()[1]
    errors = []

    def run():
        try:
            connection, _ = listener.accept()
            with connection:
                connection.settimeout(3)
                ready._read_frame(connection)  # Handshake.
                assert ready._read_frame(connection) == b"\x00"  # Status request.
                if malformed:
                    connection.sendall(ready._varint(ready.MAX_STATUS_FRAME + 1))
                else:
                    body = json.dumps(document if document is not None else {
                        "version": {"protocol": protocol, "name": "1.7.10"},
                        "description": {"text": label},
                        "players": {"max": 8, "online": 0},
                    }).encode("utf-8")
                    connection.sendall(ready._frame(b"\x00" + ready._varint(len(body)) + body))
        except Exception as error:
            errors.append(error)
        finally:
            listener.close()

    thread = threading.Thread(target=run)
    thread.start()
    try:
        yield port
    finally:
        thread.join(timeout=4)
        if thread.is_alive():
            raise AssertionError("test server thread did not terminate")
        if errors:
            raise errors[0]


class ReadinessTests(unittest.TestCase):
    def test_valid_legacy_status_counts_as_ready(self):
        with status_server() as port:
            self.assertEqual(ready.probe("127.0.0.1", port)["version"]["protocol"], 5)

    def test_malformed_status_json_is_not_ready(self):
        with status_server(document=["not a status object"]) as port:
            with self.assertRaisesRegex(ValueError, "malformed status object"):
                ready.probe("127.0.0.1", port)

    def test_wrong_protocol_is_not_ready(self):
        with status_server(protocol=47) as port:
            with self.assertRaisesRegex(ValueError, "Protocol 5"):
                ready.probe("127.0.0.1", port)

    def test_other_listener_is_not_ready(self):
        with status_server(label="Another server") as port:
            with self.assertRaisesRegex(ValueError, "not IndustrialRust1710"):
                ready.probe("127.0.0.1", port)

    def test_oversized_response_is_not_ready(self):
        with status_server(malformed=True) as port:
            with self.assertRaisesRegex(ValueError, "frame length"):
                ready.probe("127.0.0.1", port)

    def test_offline_port_expires_without_false_ready(self):
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
        listener.close()
        with self.assertRaises(TimeoutError):
            ready.wait_for_ready("127.0.0.1", port, timeout=0.05, interval=0.01)

    def test_rejects_invalid_cli_inputs(self):
        self.assertEqual(ready.main(["--port", "0", "--timeout", "0.01"]), 1)
        self.assertEqual(ready.main(["--timeout", "0"]), 1)


if __name__ == "__main__":
    unittest.main()

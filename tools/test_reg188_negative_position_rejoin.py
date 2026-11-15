"""REG-188 negative-coordinate verifier helper tests; no native server."""
import json
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest import TestCase

import verify_reg188_negative_position_rejoin as audit


class NegativeCoordinateVerifierTests(TestCase):
    def test_exact_saved_itemstack_count(self):
        with TemporaryDirectory() as scratch:
            path = Path(scratch) / "positioner.inventory.json"
            slots = [None] * 45
            slots[36] = {"id": 1, "count": 6, "damage": 0}
            slots[37] = {"id": 1, "count": 2, "damage": 0}
            slots[38] = {"id": 3, "count": 44, "damage": 0}
            path.write_text(json.dumps({"version": 1, "slots": slots}))
            self.assertEqual(audit.inventory_count(path, 1), 8)
            self.assertEqual(audit.inventory_count(path, 3), 44)
            slots.pop()
            path.write_text(json.dumps({"version": 1, "slots": slots}))
            with self.assertRaises(ValueError):
                audit.inventory_count(path, 1)

    def test_negative_coordinate_match_is_strict(self):
        for coordinate in audit.NEGATIVE:
            self.assertTrue(audit.position_matches(
                (coordinate, 65.0, coordinate),
                coordinate, 65.0, coordinate,
            ))
            self.assertFalse(audit.position_matches(
                (coordinate + 1.0, 65.0, coordinate),
                coordinate, 65.0, coordinate,
            ))
        self.assertFalse(audit.position_matches(None, -1, 65, -1))

    def test_server_feet_undoes_the_s08_eye_offset(self):
        # S08 Y is the client eye height (server feet + 1.62); the verifier
        # must compare feet coordinates, exactly like read_position does in
        # the Rust Protocol 5 suite.
        self.assertAlmostEqual(audit.server_feet((0.5, 65.62, -16.5))[1], 64.0, places=9)
        self.assertEqual(audit.server_feet((0.5, 65.62, -16.5))[0::2], (0.5, -16.5))
        self.assertIsNone(audit.server_feet(None))

    def test_output_must_be_new(self):
        with TemporaryDirectory() as path:
            with self.assertRaises(FileExistsError):
                audit.new_output_directory(Path(path))

"""Independent development-only Java-long oracle for A01 GenLayerSmooth fixtures.

This does not run or supervise the Minecraft server. The Rust tests contain
fixed expected grids; this verifies those fixtures without executing Rust.
"""
import unittest

MASK64 = (1 << 64) - 1
MUL = 6364136223846793005
ADD = 1442695040888963407


def signed64(value):
    value &= MASK64
    return value if value < (1 << 63) else value - (1 << 64)


def signed32(value):
    value &= (1 << 32) - 1
    return value if value < (1 << 31) else value - (1 << 32)


def advance(value, salt):
    return signed64(value * (value * MUL + ADD) + salt)


def mixed_world_seed(seed, salt):
    base = salt
    for _ in range(3):
        base = advance(base, salt)
    for _ in range(3):
        seed = advance(seed, base)
    return seed


def parent(x, z):
    x, z = signed32(x), signed32(z)
    return (x & 1) + ((z & 1) << 1) + (4 if ((x ^ z) & 7) == 0 else 0)


def smooth_cell(world, x, z):
    x, z = signed32(x), signed32(z)
    west, east = parent(x - 1, z), parent(x + 1, z)
    north, south = parent(x, z - 1), parent(x, z + 1)
    center = parent(x, z)
    if west == east and north == south:
        chunk = world
        for coord in (x, z, x, z):
            chunk = advance(chunk, coord)
        return west if (chunk >> 24) % 2 == 0 else north
    if west == east:
        center = west
    if north == south:
        center = north
    return center


def fixture(seed, x, z):
    world = mixed_world_seed(seed, 1000)
    return "".join(
        f"{smooth_cell(world, x + dx, z + dz):02x}"
        for dz in range(8) for dx in range(8)
    )


class GenLayerSmoothOracleTest(unittest.TestCase):
    def test_fixed_seed_grids_match_java_21_reference(self):
        # These constants were produced independently with javac/java 21.
        cases = (
            (42, -17, -17,
             "0102020301000202010101020301000101020102020002030301010201010002"
             "0100010201020103030100020101010102000203020201020101000103010101"),
            (-12345, 15, -17,
             "0102010301030202010201020302030101020202020002030001010201010001"
             "0200020202020200030103020101010101030103020201020102000103010101"),
            ((1 << 63) - 1, -1, 15,
             "0102020302030202010101020001030101020102010302000001010101010001"
             "0203010201020103000200020102010101000100020201020101030200020102"),
            (42, (1 << 31) - 3, -(1 << 31) + 1,
             "0200010201020203030103010101010202030100020202020101030103020101"
             "0102010301000202010101010302000202020102020301000001010101010302"),
        )
        for seed, x, z, expected in cases:
            with self.subTest(seed=seed, x=x, z=z):
                self.assertEqual(fixture(seed, x, z), expected)

    def test_coordinate_wrap_and_split_read(self):
        seed = mixed_world_seed(42, 1000)
        x, z = (1 << 31) - 3, -(1 << 31) + 1
        whole = [smooth_cell(seed, x + dx, z + dz)
                 for dz in range(8) for dx in range(8)]
        self.assertEqual(len(whole), 64)
        for dz in reversed(range(8)):
            for dx in reversed(range(8)):
                self.assertEqual(whole[dz * 8 + dx],
                                 smooth_cell(seed, signed32(x + dx), signed32(z + dz)))


if __name__ == "__main__":
    unittest.main()

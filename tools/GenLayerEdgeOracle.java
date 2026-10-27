/**
 * Independent development-only Java oracle for Minecraft 1.7.10 GenLayerEdge.
 * Run: java tools/GenLayerEdgeOracle.java
 * This is not a server/runtime implementation.
 */
public final class GenLayerEdgeOracle {
    private static final long MULTIPLIER = 6364136223846793005L;
    private static final long ADDEND = 1442695040888963407L;

    private static long advance(long value, long salt) {
        return value * (value * MULTIPLIER + ADDEND) + salt;
    }

    private static final class Rng {
        long worldSeed;
        long chunkSeed;

        Rng(long seed, long salt) {
            long base = salt;
            for (int i = 0; i < 3; i++) {
                base = advance(base, salt);
            }
            worldSeed = seed;
            for (int i = 0; i < 3; i++) {
                worldSeed = advance(worldSeed, base);
            }
        }

        void start(int x, int z) {
            chunkSeed = worldSeed;
            chunkSeed = advance(chunkSeed, x);
            chunkSeed = advance(chunkSeed, z);
            chunkSeed = advance(chunkSeed, x);
            chunkSeed = advance(chunkSeed, z);
        }

        int next(int bound) {
            int result = (int) ((chunkSeed >> 24) % bound);
            if (result < 0) {
                result += bound;
            }
            chunkSeed = advance(chunkSeed, worldSeed);
            return result;
        }
    }

    private static int parent(int x, int z) {
        switch ((x ^ z) & 7) {
            case 0: return 0;
            case 1: return 1;
            case 2: return 3;
            case 3: return 4;
            case 4: return 2;
            case 5: return 1;
            case 6: return 4;
            default: return 3;
        }
    }

    private static int cell(int mode, Rng rng, int x, int z) {
        int value = parent(x, z);
        if (mode == 0 && value == 1) {
            if (parent(x, z - 1) == 3 || parent(x + 1, z) == 3
                    || parent(x - 1, z) == 3 || parent(x, z + 1) == 3
                    || parent(x, z - 1) == 4 || parent(x + 1, z) == 4
                    || parent(x - 1, z) == 4 || parent(x, z + 1) == 4) {
                value = 2;
            }
        } else if (mode == 1 && value == 4) {
            if (parent(x, z - 1) == 2 || parent(x + 1, z) == 2
                    || parent(x - 1, z) == 2 || parent(x, z + 1) == 2
                    || parent(x, z - 1) == 1 || parent(x + 1, z) == 1
                    || parent(x - 1, z) == 1 || parent(x, z + 1) == 1) {
                value = 3;
            }
        } else if (mode == 2) {
            rng.start(x, z);
            if (value != 0 && rng.next(13) == 0) {
                value |= ((1 + rng.next(15)) << 8) & 3840;
            }
        }
        return value;
    }

    public static void main(String[] args) {
        long[] seeds = {42L, -12345L, Long.MAX_VALUE};
        int[] xs = {-17, 15, -1};
        int[] zs = {-17, -17, -1};
        for (int mode = 0; mode < 3; mode++) {
            for (int i = 0; i < seeds.length; i++) {
                Rng rng = new Rng(seeds[i], mode == 2 ? 3L : 2L);
                System.out.printf("mode=%d seed=%d x=%d z=%d%n",
                        mode, seeds[i], xs[i], zs[i]);
                for (int dz = 0; dz < 8; dz++) {
                    StringBuilder row = new StringBuilder();
                    for (int dx = 0; dx < 8; dx++) {
                        row.append(String.format("%04x",
                                cell(mode, rng, xs[i] + dx, zs[i] + dz)));
                    }
                    System.out.println(row);
                }
            }
        }
    }
}

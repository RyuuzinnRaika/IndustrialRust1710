/**
 * Standalone Java-long reference for the 1.7.10 Island -> AddIsland fixture.
 * No Minecraft runtime or external dependencies required.
 *
 * Run: java tools/GenLayerAddIslandOracle.java
 * Expected: four lines of eight 8-digit rows joined with '/' separators.
 */
public final class GenLayerAddIslandOracle {
    private static final long MULT = 6364136223846793005L;
    private static final long ADD = 1442695040888963407L;

    private static long step(long value, long salt) {
        return value * (value * MULT + ADD) + salt;
    }

    private static final class RandomLayer {
        private final long worldSeed;
        private long chunkSeed;

        RandomLayer(long world, long salt) {
            long base = salt;
            for (int i = 0; i < 3; i++) base = step(base, salt);
            for (int i = 0; i < 3; i++) world = step(world, base);
            this.worldSeed = world;
            this.chunkSeed = world;
        }

        void at(long x, long z) {
            chunkSeed = worldSeed;
            chunkSeed = step(chunkSeed, x);
            chunkSeed = step(chunkSeed, z);
            chunkSeed = step(chunkSeed, x);
            chunkSeed = step(chunkSeed, z);
        }

        int next(int bound) {
            int result = (int) ((chunkSeed >> 24) % bound);
            if (result < 0) result += bound;
            chunkSeed = step(chunkSeed, worldSeed);
            return result;
        }
    }

    private static int[] island(long world, int x, int z, int width, int height) {
        int[] values = new int[width * height];
        RandomLayer rng = new RandomLayer(world, 1);
        for (int dz = 0; dz < height; dz++) {
            for (int dx = 0; dx < width; dx++) {
                rng.at((long) x + dx, (long) z + dz);
                values[dz * width + dx] = rng.next(10) == 0 ? 1 : 0;
            }
        }
        long ox = -(long) x;
        long oz = -(long) z;
        if (ox >= 0 && oz >= 0 && ox < width && oz < height) {
            values[(int) oz * width + (int) ox] = 1;
        }
        return values;
    }

    private static int[] addIsland(long world, long salt, int x, int z, int width, int height) {
        int pw = width + 2;
        int[] parent = island(world, x - 1, z - 1, pw, height + 2);
        int[] output = new int[width * height];
        RandomLayer rng = new RandomLayer(world, salt);
        for (int dz = 0; dz < height; dz++) {
            for (int dx = 0; dx < width; dx++) {
                int nw = parent[dz * pw + dx];
                int ne = parent[dz * pw + dx + 2];
                int sw = parent[(dz + 2) * pw + dx];
                int se = parent[(dz + 2) * pw + dx + 2];
                int center = parent[(dz + 1) * pw + dx + 1];
                rng.at((long) x + dx, (long) z + dz);
                int value = center;
                if (center == 0 && (nw != 0 || ne != 0 || sw != 0 || se != 0)) {
                    int chosen = 1;
                    int count = 1;
                    for (int neighbor : new int[]{nw, ne, sw, se}) {
                        if (neighbor != 0 && rng.next(count++) == 0) chosen = neighbor;
                    }
                    value = (rng.next(3) == 0 || chosen == 4) ? chosen : 0;
                } else if (center > 0 && (nw == 0 || ne == 0 || sw == 0 || se == 0)) {
                    value = rng.next(5) == 0 && center != 4 ? 0 : center;
                }
                output[dz * width + dx] = value;
            }
        }
        return output;
    }

    public static void main(String[] args) {
        long[][] cases = {
            {-12345, 1, -17, 7},
            {0, 1, -1, -1},
            {42, 1, 15, -17},
            {42, 2, -17, -17}
        };
        for (long[] test : cases) {
            int[] values = addIsland(test[0], test[1], (int) test[2], (int) test[3], 8, 8);
            StringBuilder out = new StringBuilder();
            for (int i = 0; i < values.length; i++) {
                if (i != 0 && i % 8 == 0) out.append('/');
                out.append(values[i]);
            }
            System.out.println(out);
        }
    }
}

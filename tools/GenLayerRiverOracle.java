/** Reproduce Rust RiverInit golden vectors using Java-long overflow.
 * Run: java tools/GenLayerRiverOracle.java
 * No Minecraft jars or dependencies required.
 */
public final class GenLayerRiverOracle {
    private static final long M = 6364136223846793005L;
    private static final long A = 1442695040888963407L;

    private static long mix(long s, long salt) {
        return s * (s * M + A) + salt;
    }

    private static long world(long seed, long salt) {
        long base = salt;
        for (int i = 0; i < 3; i++) base = mix(base, salt);
        for (int i = 0; i < 3; i++) seed = mix(seed, base);
        return seed;
    }

    private static int next(long seed, long x, long z) {
        long chunk = seed;
        chunk = mix(chunk, x);
        chunk = mix(chunk, z);
        chunk = mix(chunk, x);
        chunk = mix(chunk, z);
        int value = (int) ((chunk >> 24) % 299999L);
        if (value < 0) value += 299999;
        return value + 2;
    }

    private static int parent(int x, int z) {
        return ((x ^ z) & 3) == 0 ? 0 : 1;
    }

    public static void main(String[] args) {
        long[][] cases = {
            {42, 100, -17, 7},
            {-12345, 100, -1, -1},
            {0, 100, 15, -17}
        };
        for (long[] test : cases) {
            long seed = world(test[0], test[1]);
            StringBuilder row = new StringBuilder();
            for (int dz = 0; dz < 4; dz++) {
                for (int dx = 0; dx < 4; dx++) {
                    int x = (int) test[2] + dx;
                    int z = (int) test[3] + dz;
                    row.append(parent(x, z) == 0 ? 0 : next(seed, x, z));
                    if (dx < 3) row.append(',');
                }
                if (dz < 3) row.append('/');
            }
            System.out.println(row);
        }
    }
}

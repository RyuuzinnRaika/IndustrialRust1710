/** Standalone Java-long oracle for Minecraft 1.7.10 GenLayerAddSnow.
 * Run: java tools/GenLayerAddSnowOracle.java
 * This deliberately does not depend on the Rust implementation or Minecraft jars.
 */
public final class GenLayerAddSnowOracle {
    private static final long M = 6364136223846793005L;
    private static final long A = 1442695040888963407L;

    private static long advance(long seed, long salt) {
        return seed * (seed * M + A) + salt;
    }

    private static long world(long seed, long salt) {
        long base = salt;
        for (int i = 0; i < 3; i++) base = advance(base, salt);
        for (int i = 0; i < 3; i++) seed = advance(seed, base);
        return seed;
    }

    private static int climate(long seed, int x, int z) {
        if (((x ^ z) & 7) == 0) return 0;
        long chunk = seed;
        chunk = advance(chunk, x);
        chunk = advance(chunk, z);
        chunk = advance(chunk, x);
        chunk = advance(chunk, z);
        int roll = (int) ((chunk >> 24) % 6L);
        if (roll < 0) roll += 6;
        return roll == 0 ? 4 : roll <= 1 ? 3 : 1;
    }

    public static void main(String[] args) {
        long[][] cases = {
            {42, 2, -17, 7},
            {-12345, 2, -17, -17},
            {0, 2, 15, -17},
            {Long.MAX_VALUE, 2, -1, -1}
        };
        for (long[] test : cases) {
            long seed = world(test[0], test[1]);
            StringBuilder rows = new StringBuilder();
            for (int dz = 0; dz < 8; dz++) {
                for (int dx = 0; dx < 8; dx++) {
                    rows.append(climate(seed, (int) test[2] + dx, (int) test[3] + dz));
                }
                if (dz < 7) rows.append('/');
            }
            System.out.println(test[0] + " " + test[2] + " " + test[3] + " " + rows);
        }
    }
}

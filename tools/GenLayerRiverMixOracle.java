/** Standalone Java 1.7.10 GenLayerRiverMix branch oracle (no Minecraft dependency).
 * Run: javac tools/GenLayerRiverMixOracle.java && java -cp tools GenLayerRiverMixOracle
 */
public final class GenLayerRiverMixOracle {
    private static int[] mix(int[] biome, int[] river) {
        int[] result = new int[biome.length];
        for (int i = 0; i < biome.length; i++) {
            if (biome[i] != 0 && biome[i] != 24) {
                if (river[i] == 7) {
                    if (biome[i] == 12) {
                        result[i] = 11;
                    } else if (biome[i] != 14 && biome[i] != 15) {
                        result[i] = river[i] & 255;
                    } else {
                        result[i] = 15;
                    }
                } else {
                    result[i] = biome[i];
                }
            } else {
                result[i] = biome[i];
            }
        }
        return result;
    }

    public static void main(String[] args) {
        int[] biome = {0, 24, 12, 14, 15, 1, 4, 3, 30, 2, 7, 11, 13, 16, 21, 35};
        int[] river = {7, 7, 7, 7, 7, 7, -1, 7, 7, 7, -1, 7, 7, 7, 7, 7};
        System.out.println(java.util.Arrays.toString(mix(biome, river)));
    }
}

package studio.auru.pm;

import java.util.Arrays;

/**
 * BLAKE3, as used for every hash in {@code auru-pm-v1}.
 *
 * <p>Implemented here rather than taken from a dependency because this library has no runtime
 * dependencies at all, and because a hash is the one thing you can verify completely: the
 * published vectors in {@code spec/vectors/content-hash.json} cover every length where BLAKE3's
 * structure changes — the 64-byte compression block, the 1024-byte chunk, and each point where
 * the Merkle tree gains a level.
 *
 * <p>Only the plain hash is implemented. Keyed hashing and key derivation are part of BLAKE3 but
 * no part of this protocol, and code that is never exercised is code that is never correct.
 */
final class Blake3 {

    private static final int OUT_LEN = 32;
    private static final int KEY_LEN = 8;
    private static final int BLOCK_LEN = 64;
    private static final int CHUNK_LEN = 1024;

    private static final int CHUNK_START = 1;
    private static final int CHUNK_END = 2;
    private static final int PARENT = 4;
    private static final int ROOT = 8;

    private static final int[] IV = {
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    };

    private static final int[] MSG_PERMUTATION = {
        2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8,
    };

    private Blake3() {}

    /** The 32-byte hash of {@code input}. */
    static byte[] hash(byte[] input) {
        Hasher hasher = new Hasher();
        hasher.update(input, 0, input.length);
        return hasher.finish();
    }

    // ── The compression function ─────────────────────────────────────────────

    private static void g(int[] state, int a, int b, int c, int d, int mx, int my) {
        state[a] = state[a] + state[b] + mx;
        state[d] = Integer.rotateRight(state[d] ^ state[a], 16);
        state[c] = state[c] + state[d];
        state[b] = Integer.rotateRight(state[b] ^ state[c], 12);
        state[a] = state[a] + state[b] + my;
        state[d] = Integer.rotateRight(state[d] ^ state[a], 8);
        state[c] = state[c] + state[d];
        state[b] = Integer.rotateRight(state[b] ^ state[c], 7);
    }

    private static void round(int[] state, int[] m) {
        // Columns.
        g(state, 0, 4, 8, 12, m[0], m[1]);
        g(state, 1, 5, 9, 13, m[2], m[3]);
        g(state, 2, 6, 10, 14, m[4], m[5]);
        g(state, 3, 7, 11, 15, m[6], m[7]);
        // Diagonals.
        g(state, 0, 5, 10, 15, m[8], m[9]);
        g(state, 1, 6, 11, 12, m[10], m[11]);
        g(state, 2, 7, 8, 13, m[12], m[13]);
        g(state, 3, 4, 9, 14, m[14], m[15]);
    }

    private static void permute(int[] m) {
        int[] permuted = new int[16];
        for (int index = 0; index < 16; index++) {
            permuted[index] = m[MSG_PERMUTATION[index]];
        }
        System.arraycopy(permuted, 0, m, 0, 16);
    }

    /** The 16-word output of compressing one block. */
    private static int[] compress(
            int[] chainingValue, int[] blockWords, long counter, int blockLen, int flags) {
        int[] state = {
            chainingValue[0], chainingValue[1], chainingValue[2], chainingValue[3],
            chainingValue[4], chainingValue[5], chainingValue[6], chainingValue[7],
            IV[0], IV[1], IV[2], IV[3],
            (int) counter, (int) (counter >>> 32), blockLen, flags,
        };
        int[] block = blockWords.clone();

        for (int index = 0; index < 7; index++) {
            round(state, block);
            if (index < 6) {
                permute(block);
            }
        }

        for (int index = 0; index < 8; index++) {
            state[index] ^= state[index + 8];
            state[index + 8] ^= chainingValue[index];
        }
        return state;
    }

    private static int[] wordsFromBlock(byte[] block) {
        int[] words = new int[16];
        for (int index = 0; index < 16; index++) {
            int offset = index * 4;
            words[index] =
                    (block[offset] & 0xff)
                            | ((block[offset + 1] & 0xff) << 8)
                            | ((block[offset + 2] & 0xff) << 16)
                            | ((block[offset + 3] & 0xff) << 24);
        }
        return words;
    }

    /**
     * A node's compression inputs, kept unevaluated.
     *
     * <p>Whether a node is the root is only known once the input ends, and the root is compressed
     * with an extra flag — so a node's chaining value and its root output are different results
     * from the same inputs, and both have to stay reachable.
     */
    private static final class Output {
        private final int[] inputChainingValue;
        private final int[] blockWords;
        private final long counter;
        private final int blockLen;
        private final int flags;

        Output(int[] inputChainingValue, int[] blockWords, long counter, int blockLen, int flags) {
            this.inputChainingValue = inputChainingValue;
            this.blockWords = blockWords;
            this.counter = counter;
            this.blockLen = blockLen;
            this.flags = flags;
        }

        int[] chainingValue() {
            return Arrays.copyOf(
                    compress(inputChainingValue, blockWords, counter, blockLen, flags), 8);
        }

        byte[] rootBytes() {
            int[] words = compress(inputChainingValue, blockWords, 0, blockLen, flags | ROOT);
            byte[] out = new byte[OUT_LEN];
            for (int index = 0; index < OUT_LEN / 4; index++) {
                int word = words[index];
                out[index * 4] = (byte) word;
                out[index * 4 + 1] = (byte) (word >>> 8);
                out[index * 4 + 2] = (byte) (word >>> 16);
                out[index * 4 + 3] = (byte) (word >>> 24);
            }
            return out;
        }
    }

    /** One 1024-byte chunk, accumulated a block at a time. */
    private static final class ChunkState {
        private int[] chainingValue;
        private final long chunkCounter;
        private final byte[] block = new byte[BLOCK_LEN];
        private int blockLen;
        private int blocksCompressed;

        ChunkState(int[] key, long chunkCounter) {
            this.chainingValue = key.clone();
            this.chunkCounter = chunkCounter;
        }

        int length() {
            return BLOCK_LEN * blocksCompressed + blockLen;
        }

        private int startFlag() {
            return blocksCompressed == 0 ? CHUNK_START : 0;
        }

        void update(byte[] input, int offset, int length) {
            int position = offset;
            int remaining = length;
            while (remaining > 0) {
                if (blockLen == BLOCK_LEN) {
                    // A full block is only compressed once more input arrives:
                    // the final block of a chunk carries CHUNK_END, and whether
                    // this is the final one is not yet known.
                    chainingValue =
                            Arrays.copyOf(
                                    compress(
                                            chainingValue,
                                            wordsFromBlock(block),
                                            chunkCounter,
                                            BLOCK_LEN,
                                            startFlag()),
                                    8);
                    blocksCompressed++;
                    Arrays.fill(block, (byte) 0);
                    blockLen = 0;
                }

                int take = Math.min(BLOCK_LEN - blockLen, remaining);
                System.arraycopy(input, position, block, blockLen, take);
                blockLen += take;
                position += take;
                remaining -= take;
            }
        }

        Output output() {
            return new Output(
                    chainingValue,
                    wordsFromBlock(block),
                    chunkCounter,
                    blockLen,
                    startFlag() | CHUNK_END);
        }
    }

    private static Output parentOutput(int[] left, int[] right, int[] key) {
        int[] blockWords = new int[16];
        System.arraycopy(left, 0, blockWords, 0, 8);
        System.arraycopy(right, 0, blockWords, 8, 8);
        return new Output(key.clone(), blockWords, 0, BLOCK_LEN, PARENT);
    }

    /** Incremental hashing, so a large blob need not be held in memory at once. */
    static final class Hasher {
        private ChunkState chunkState = new ChunkState(IV, 0);
        private final int[][] cvStack = new int[54][];
        private int cvStackLen;

        /**
         * Fold a completed chunk into the tree.
         *
         * <p>Every trailing zero bit of the chunk count is one subtree that just became complete,
         * so this merges exactly that many — which is what keeps the stack shallow enough for the
         * fixed bound above.
         */
        private void addChunkChainingValue(int[] chainingValue, long totalChunks) {
            int[] value = chainingValue;
            long remaining = totalChunks;
            while ((remaining & 1) == 0) {
                value = parentOutput(cvStack[--cvStackLen], value, IV).chainingValue();
                remaining >>= 1;
            }
            cvStack[cvStackLen++] = value;
        }

        void update(byte[] input, int offset, int length) {
            int position = offset;
            int remaining = length;
            while (remaining > 0) {
                if (chunkState.length() == CHUNK_LEN) {
                    long totalChunks = chunkState.chunkCounter + 1;
                    addChunkChainingValue(chunkState.output().chainingValue(), totalChunks);
                    chunkState = new ChunkState(IV, totalChunks);
                }

                int take = Math.min(CHUNK_LEN - chunkState.length(), remaining);
                chunkState.update(input, position, take);
                position += take;
                remaining -= take;
            }
        }

        byte[] finish() {
            Output output = chunkState.output();
            for (int index = cvStackLen - 1; index >= 0; index--) {
                output = parentOutput(cvStack[index], output.chainingValue(), IV);
            }
            return output.rootBytes();
        }
    }

    static {
        assert IV.length == KEY_LEN;
    }
}

package studio.auru.pm;

import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import java.util.regex.Pattern;

/**
 * A BLAKE3 content hash, in the canonical {@code blake3:<64 lowercase hex>} form.
 *
 * <p>Every hash on the wire uses this shape, commit ids included. The type exists so a hash and an
 * arbitrary string cannot be confused at a call site: passing the wrong one produces a compile
 * error rather than a request a provider rejects.
 */
public final class ContentHash {

    private static final Pattern CANONICAL = Pattern.compile("^blake3:[0-9a-f]{64}$");
    private static final char[] HEX = "0123456789abcdef".toCharArray();

    private final byte[] bytes;

    private ContentHash(byte[] bytes) {
        this.bytes = bytes;
    }

    /** The hash of {@code data}. */
    public static ContentHash of(byte[] data) {
        return new ContentHash(Blake3.hash(data));
    }

    /** The hash of {@code text}, encoded as UTF-8. */
    public static ContentHash of(String text) {
        return of(text.getBytes(StandardCharsets.UTF_8));
    }

    /**
     * Parse the canonical string form.
     *
     * @throws IllegalArgumentException if {@code text} is not {@code blake3:} followed by 64
     *     lowercase hex digits. Uppercase is rejected rather than normalized: two spellings of one
     *     hash would compare unequal as strings, and something downstream would eventually compare
     *     them as strings.
     */
    public static ContentHash parse(String text) {
        if (!CANONICAL.matcher(text).matches()) {
            throw new IllegalArgumentException(
                    "not a canonical content hash: \"" + text + "\" (expected blake3:<64 lowercase hex>)");
        }
        byte[] bytes = new byte[32];
        for (int index = 0; index < 32; index++) {
            int offset = "blake3:".length() + index * 2;
            bytes[index] = (byte) Integer.parseInt(text.substring(offset, offset + 2), 16);
        }
        return new ContentHash(bytes);
    }

    /** The raw 32 bytes. */
    public byte[] toByteArray() {
        return bytes.clone();
    }

    /** Whether {@code data} hashes to this. The check a client owes itself on every download. */
    public boolean matches(byte[] data) {
        return Arrays.equals(bytes, Blake3.hash(data));
    }

    @Override
    public String toString() {
        StringBuilder text = new StringBuilder(7 + 64).append("blake3:");
        for (byte value : bytes) {
            text.append(HEX[(value >> 4) & 0xf]).append(HEX[value & 0xf]);
        }
        return text.toString();
    }

    @Override
    public boolean equals(Object other) {
        return other instanceof ContentHash hash && Arrays.equals(bytes, hash.bytes);
    }

    @Override
    public int hashCode() {
        return Arrays.hashCode(bytes);
    }
}

package studio.auru.pm;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.regex.Matcher;
import java.util.regex.Pattern;
import org.junit.jupiter.api.DynamicTest;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.TestFactory;

/**
 * BLAKE3 against the published vectors.
 *
 * <p>The vector file is read with a regex rather than through this library's own JSON parser, so a
 * failure here means the hash is wrong and nothing else. Two components that can only be tested
 * together can only be debugged together.
 */
class Blake3Test {

    private static final Pattern CASE =
            Pattern.compile("\\{\\s*\"hash\":\\s*\"(blake3:[0-9a-f]{64})\",\\s*\"length\":\\s*(\\d+)\\s*}");

    private record Vector(int length, String hash) {}

    private static List<Vector> vectors() throws IOException {
        Path path =
                Path.of(System.getProperty("auru.repoRoot"), "spec/vectors/content-hash.json");
        Matcher matcher = CASE.matcher(Files.readString(path));
        List<Vector> vectors = new ArrayList<>();
        while (matcher.find()) {
            vectors.add(new Vector(Integer.parseInt(matcher.group(2)), matcher.group(1)));
        }
        assertTrue(vectors.size() >= 30, "expected the published vectors, found " + vectors.size());
        return vectors;
    }

    /** Byte {@code i} is {@code i % 251}, the pattern BLAKE3's own test suite uses. */
    private static byte[] input(int length) {
        byte[] bytes = new byte[length];
        for (int index = 0; index < length; index++) {
            bytes[index] = (byte) (index % 251);
        }
        return bytes;
    }

    @TestFactory
    List<DynamicTest> reproducesEveryPublishedVector() throws IOException {
        List<DynamicTest> tests = new ArrayList<>();
        for (Vector vector : vectors()) {
            tests.add(
                    DynamicTest.dynamicTest(
                            "input of " + vector.length() + " bytes",
                            () ->
                                    assertEquals(
                                            vector.hash(),
                                            ContentHash.of(input(vector.length())).toString())));
        }
        return tests;
    }

    @Test
    void coversTheChunkAndTreeBoundaries() throws IOException {
        List<Integer> lengths = vectors().stream().map(Vector::length).toList();
        // A port that only handles one chunk would pass a careless vector list.
        for (int boundary : new int[] {64, 1024, 1025, 2048, 4096}) {
            assertTrue(lengths.contains(boundary), "missing the " + boundary + "-byte boundary");
        }
        assertTrue(lengths.stream().anyMatch(length -> length > 65_536));
    }

    @Test
    void hashesTheEmptyInputToTheKnownValue() {
        assertEquals(
                "blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
                ContentHash.of(new byte[0]).toString());
    }

    @Test
    void parsesAndRendersTheCanonicalForm() {
        String text = "blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";
        assertEquals(text, ContentHash.parse(text).toString());
        assertEquals(ContentHash.of(new byte[0]), ContentHash.parse(text));
    }

    @Test
    void rejectsAnythingThatIsNotTheCanonicalForm() {
        assertThrows(IllegalArgumentException.class, () -> ContentHash.parse("deadbeef"));
        assertThrows(
                IllegalArgumentException.class,
                () -> ContentHash.parse("sha256:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"));
        // Uppercase would compare unequal as a string to the same hash in lowercase.
        assertThrows(
                IllegalArgumentException.class,
                () -> ContentHash.parse("blake3:AF1349B9F5F9A1A6A0404DEA36DCC9499BCB25C9ADC112B7CC9A93CAE41F3262"));
    }

    @Test
    void verifiesBytesAgainstTheirOwnName() {
        byte[] audio = "kick.wav".getBytes(java.nio.charset.StandardCharsets.UTF_8);
        ContentHash hash = ContentHash.of(audio);
        assertTrue(hash.matches(audio));
        assertFalse(hash.matches("snare.wav".getBytes(java.nio.charset.StandardCharsets.UTF_8)));
    }
}

package studio.auru.pm;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import org.junit.jupiter.api.DynamicTest;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.TestFactory;

/**
 * The conformance gate.
 *
 * <p>A commit whose id this library cannot reproduce is rejected by every provider, so these are
 * not stylistic assertions — they are the contract. The cases come from
 * {@code spec/vectors/commit-encoding.json}, generated from the Rust implementation, and the same
 * file is what the TypeScript SDK is checked against.
 */
class CommitVectorsTest {

    private static Json vectors() throws IOException {
        Path path = Path.of(System.getProperty("auru.repoRoot"), "spec/vectors/commit-encoding.json");
        return Json.parse(Files.readString(path));
    }

    private static List<Json> cases() throws IOException {
        List<Json> cases = vectors().require("cases").elements();
        assertTrue(cases.size() >= 10, "expected the published vectors, found " + cases.size());
        return cases;
    }

    @Test
    void publishesTheRuleItIsCheckedAgainst() throws IOException {
        assertTrue(vectors().string("rule").contains("RFC 8785"));
    }

    @TestFactory
    List<DynamicTest> reproducesTheCanonicalBytes() throws IOException {
        List<DynamicTest> tests = new ArrayList<>();
        for (Json testCase : cases()) {
            String name = testCase.string("name");
            tests.add(
                    DynamicTest.dynamicTest(
                            name,
                            () -> {
                                Commit commit = Commit.fromJson(testCase.require("commit"));
                                assertEquals(
                                        testCase.string("canonical"),
                                        new String(commit.canonicalEncoding(), StandardCharsets.UTF_8),
                                        "canonical bytes differ for " + name);
                            }));
        }
        return tests;
    }

    @TestFactory
    List<DynamicTest> derivesTheRecordedId() throws IOException {
        List<DynamicTest> tests = new ArrayList<>();
        for (Json testCase : cases()) {
            String name = testCase.string("name");
            tests.add(
                    DynamicTest.dynamicTest(
                            name,
                            () -> {
                                Commit commit = Commit.fromJson(testCase.require("commit"));
                                assertEquals(testCase.string("id"), commit.id().toString());
                                assertTrue(commit.verifyId(), "id does not verify for " + name);
                            }));
        }
        return tests;
    }

    @Test
    void ignoresTheIdAlreadyOnACommit() throws IOException {
        // Identity is a function of content, not of itself.
        Json first = cases().get(0);
        Commit commit = Commit.fromJson(first.require("commit"));
        Commit rebuilt =
                Commit.builder()
                        .parents(commit.parents())
                        .tree(commit.tree())
                        .author(commit.author())
                        .timestamp(commit.timestamp())
                        .message(commit.message())
                        .description(commit.description())
                        .auruVersion(commit.auruVersion())
                        .formatVersion(commit.formatVersion())
                        .metadata(commit.metadata().orElse(null))
                        .build();
        assertEquals(first.string("id"), rebuilt.id().toString());
    }

    @Test
    void changesTheIdWhenAnyContentChanges() throws IOException {
        Commit commit = Commit.fromJson(cases().get(0).require("commit"));
        Commit edited =
                Commit.builder()
                        .parents(commit.parents())
                        .tree(commit.tree())
                        .author(commit.author())
                        .timestamp(commit.timestamp())
                        .message("a different message")
                        .description(commit.description())
                        .auruVersion(commit.auruVersion())
                        .formatVersion(commit.formatVersion())
                        .build();
        assertNotEquals(commit.id(), edited.id());
    }

    @Test
    void canonicalEncodingCarriesNoIdMember() throws IOException {
        for (Json testCase : cases()) {
            String canonical =
                    new String(
                            Commit.fromJson(testCase.require("commit")).canonicalEncoding(),
                            StandardCharsets.UTF_8);
            assertTrue(!canonical.contains("\"id\""), "id leaked into " + testCase.string("name"));
        }
    }

    @Test
    void canonicalizingIsIdempotent() throws IOException {
        // Re-canonicalizing already-canonical bytes must change nothing. This
        // separates a parser fault from a writer fault: a parser that loses
        // information shows up here rather than only in the byte comparison.
        for (Json testCase : cases()) {
            String canonical = testCase.string("canonical");
            assertEquals(canonical, Json.parse(canonical).toCanonicalJson());
        }
    }

    @Test
    void refusesAnIntegerOutsideWhatRfc8785CanRepresent() {
        // Beyond 2^53 two conformant implementations derive different ids, so
        // producing bytes at all here would be worse than failing.
        Json beyond = Json.object().put("timestamp", Long.MAX_VALUE).build();
        IllegalStateException error =
                assertThrows(IllegalStateException.class, beyond::toCanonicalJson);
        assertTrue(error.getMessage().contains("2^53"), error.getMessage());
    }

    @Test
    void refusesToCanonicalizeAFractionalNumber() {
        Json fractional = Json.parse("{\"tempo\":128.5}");
        IllegalStateException error =
                assertThrows(IllegalStateException.class, fractional::toCanonicalJson);
        assertTrue(error.getMessage().contains("non-integral"), error.getMessage());
    }
}

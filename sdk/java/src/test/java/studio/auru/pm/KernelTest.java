package studio.auru.pm;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.Optional;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.TestInstance;

/**
 * The native compute kernel, exercised through JNI.
 *
 * <p>Two things are being checked. That the binding is correct — the published vectors again,
 * because a marshalling bug produces plausible-looking wrong bytes rather than a crash. And that it
 * delivers what the pure client cannot: snapshot normalization, diff and merge.
 *
 * <p>Run on the host JVM rather than on a device. The JNI glue is the part that can be wrong; the
 * Android build differs only in which linker produced the library.
 */
@TestInstance(TestInstance.Lifecycle.PER_CLASS)
class KernelTest {

    @BeforeAll
    static void loadTheKernel() throws IOException, InterruptedException {
        Path repo = TestProvider.repoRoot();
        new ProcessBuilder(
                        "cargo", "build", "-p", "auru-pm-ffi", "--features", "jni", "--release",
                        "--locked", "--manifest-path", repo.resolve("Cargo.toml").toString())
                .inheritIO()
                .start()
                .waitFor();

        for (String name : List.of("libauru_pm_ffi.dylib", "libauru_pm_ffi.so", "auru_pm_ffi.dll")) {
            Path candidate = repo.resolve("target/release").resolve(name);
            if (Files.exists(candidate)) {
                Kernel.load(candidate);
                return;
            }
        }
        throw new IOException("no native kernel was built");
    }

    private static byte[] fixture() throws IOException {
        return Files.readAllBytes(
                TestProvider.repoRoot()
                        .resolve("crates/auru-pm-kernel/tests/fixtures/interchange/oracle-midi.dawproject"));
    }

    private static List<Json> vectors() throws IOException {
        return Json.parse(
                        Files.readString(
                                TestProvider.repoRoot().resolve("spec/vectors/commit-encoding.json")))
                .require("cases")
                .elements();
    }

    // ── The binding is correct ───────────────────────────────────────────────

    @Test
    void agreesWithTheClientOnTheProtocolVersion() {
        // A native artifact and a jar can drift out of step in a way neither
        // notices otherwise.
        assertEquals(Protocol.VERSION, Kernel.protocolVersion());
        assertTrue(Kernel.matchesClientProtocol());
    }

    @Test
    void reproducesEveryPublishedCommitVector() throws IOException {
        List<Json> cases = vectors();
        assertTrue(cases.size() >= 10);

        for (Json testCase : cases) {
            String name = testCase.string("name");
            String commitJson = testCase.require("commit").toJsonText();

            assertEquals(testCase.string("id"), Kernel.commitId(commitJson), name);
            assertEquals(
                    testCase.string("canonical"),
                    new String(Kernel.commitCanonicalEncoding(commitJson), StandardCharsets.UTF_8),
                    name);
        }
    }

    @Test
    void theKernelAndThePureClientDeriveTheSameID() throws IOException {
        // Two implementations of one specification. If these ever disagree, a
        // phone with the kernel and a phone without would publish commits the
        // other could not verify.
        for (Json testCase : vectors()) {
            Json json = testCase.require("commit");
            assertEquals(
                    Commit.fromJson(json).id().toString(),
                    Kernel.commitId(json.toJsonText()),
                    testCase.string("name"));
        }
    }

    @Test
    void hashesAgreeWithThePureClient() {
        for (int length : new int[] {0, 1, 63, 64, 65, 1024, 1025, 4096}) {
            byte[] bytes = new byte[length];
            for (int index = 0; index < length; index++) {
                bytes[index] = (byte) (index % 251);
            }
            assertEquals(
                    ContentHash.of(bytes).toString(), Kernel.contentHash(bytes), "length " + length);
        }
    }

    @Test
    void surfacesAnErrorAsAnExceptionRatherThanNonsense() {
        AuruKernelException error =
                assertThrows(AuruKernelException.class, () -> Kernel.commitId("not json"));
        assertTrue(error.getMessage().contains("parse commit"), error.getMessage());
    }

    @Test
    void verifiesBlobsAndDistinguishesAMalformedHash() {
        byte[] audio = "kick.wav".getBytes(StandardCharsets.UTF_8);
        assertTrue(Kernel.verify(audio, ContentHash.of(audio)));
        assertFalse(
                Kernel.verify("snare.wav".getBytes(StandardCharsets.UTF_8), ContentHash.of(audio)));
    }

    // ── What the pure client cannot do ───────────────────────────────────────

    @Test
    void normalizesARealProjectFile() throws IOException {
        byte[] source = fixture();
        String format = Kernel.detectFormat("song.dawproject", source);
        assertEquals("dawproject", format);

        byte[] snapshot = Kernel.snapshotFromSource(format, source);
        assertTrue(snapshot.length > 0);

        // Round-tripping must not change identity.
        byte[] restored = Kernel.restoreFromSnapshot(snapshot);
        byte[] again = Kernel.snapshotFromSource(format, restored);
        assertEquals(Kernel.contentHash(snapshot), Kernel.contentHash(again));
    }

    @Test
    void summarizesAProjectFarMoreCheaplyThanTheSnapshot() throws IOException {
        byte[] snapshot = Kernel.snapshotFromSource("dawproject", fixture());
        Optional<ProjectInfo> info = Kernel.projectInfo(snapshot);

        assertTrue(info.isPresent());
        assertEquals("dawproject", info.get().format());
        // The reason a commit stores this separately: a project list renders
        // without ever fetching a snapshot.
        assertTrue(info.get().detail().toJsonText().length() < snapshot.length);
    }

    @Test
    void reportsNoStructuralChangeBetweenIdenticalSnapshots() throws IOException {
        byte[] snapshot = Kernel.snapshotFromSource("dawproject", fixture());
        assertEquals(List.of("No structural changes"), Kernel.summarize(snapshot, snapshot));
    }

    @Test
    void returnsAStructuredDiffARendererCanWalk() throws IOException {
        byte[] snapshot = Kernel.snapshotFromSource("dawproject", fixture());
        Json diff = Kernel.diff(snapshot, snapshot);
        assertNotNull(diff.require("project_changes"));
        assertNotNull(diff.require("channels"));
        assertEquals(2, diff.require("time_sig").elements().size());
    }

    @Test
    void mergesDisjointEditsCleanly() {
        Json outcome =
                Kernel.merge(
                        "{\"version\":8,\"tempo\":120,\"key\":\"C\"}".getBytes(StandardCharsets.UTF_8),
                        "{\"version\":8,\"tempo\":128,\"key\":\"C\"}".getBytes(StandardCharsets.UTF_8),
                        "{\"version\":8,\"tempo\":120,\"key\":\"G\"}".getBytes(StandardCharsets.UTF_8));

        assertEquals("clean", outcome.string("outcome"));
        assertEquals(128, outcome.require("merged").integer("tempo"));
        assertEquals("G", outcome.require("merged").string("key"));
    }

    @Test
    void namesTheFieldWhenBothSidesChangedItDifferently() {
        Json outcome =
                Kernel.merge(
                        "{\"version\":8,\"tempo\":120}".getBytes(StandardCharsets.UTF_8),
                        "{\"version\":8,\"tempo\":128}".getBytes(StandardCharsets.UTF_8),
                        "{\"version\":8,\"tempo\":140}".getBytes(StandardCharsets.UTF_8));

        assertEquals("conflict", outcome.string("outcome"));
        assertEquals("tempo", outcome.require("conflicts").elements().get(0).string("path"));
    }

    @Test
    void refusesAFormatItDoesNotKnowByName() {
        AuruKernelException error =
                assertThrows(
                        AuruKernelException.class,
                        () -> Kernel.snapshotFromSource("logic-pro", "{}".getBytes(StandardCharsets.UTF_8)));
        assertTrue(error.getMessage().contains("unknown project format"), error.getMessage());
    }

    // ── Memory ───────────────────────────────────────────────────────────────

    @Test
    void repeatedCallsDoNotLeakOrCorrupt() throws IOException {
        // JNI hands every result to the JVM, so a mistake here shows as drift or
        // a crash rather than as a leak a test could measure.
        byte[] snapshot = Kernel.snapshotFromSource("dawproject", fixture());
        for (int attempt = 0; attempt < 500; attempt++) {
            Kernel.projectInfo(snapshot);
            assertThrows(AuruKernelException.class, () -> Kernel.commitId("not json"));
            Kernel.contentHash(snapshot);
        }
        assertEquals(ContentHash.of(snapshot).toString(), Kernel.contentHash(snapshot));
        assertArrayEquals(snapshot, Kernel.snapshotFromSource("dawproject", fixture()));
    }
}

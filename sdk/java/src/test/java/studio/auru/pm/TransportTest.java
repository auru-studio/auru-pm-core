package studio.auru.pm;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.List;
import java.util.Optional;
import org.junit.jupiter.api.AfterAll;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.TestInstance;

/** End-to-end against a real {@code auru-pm-server}. */
@TestInstance(TestInstance.Lifecycle.PER_CLASS)
class TransportTest {

    private static TestProvider provider;
    private static AuruClient client;
    private static AuthorIdentity author;

    @BeforeAll
    static void startProvider() throws Exception {
        provider = TestProvider.start();
        client = AuruClient.to(provider.endpoint).transport(new JdkHttpTransport()).connect();
        // A commit's author must match the identity the provider derived from
        // the token, so a client reads it once rather than inventing one.
        author = client.me().asAuthor();
    }

    @AfterAll
    static void stopProvider() throws Exception {
        if (provider != null) {
            provider.close();
        }
    }

    private static byte[] fixture() throws Exception {
        return Files.readAllBytes(
                TestProvider.repoRoot()
                        .resolve("crates/auru-pm-kernel/tests/fixtures/interchange/oracle-midi.dawproject"));
    }

    /** Create a handle, upload its blobs, and record a version. */
    private static Commit publish(
            ProjectClient project, String message, List<ContentHash> parents) throws Exception {
        // Java has no snapshot normalizer, so a version is published from bytes
        // the caller already holds — which is what a backend service does.
        ContentHash snapshot = project.putBlob(fixture());
        ContentHash samples = project.putBlob("{\"entries\":[]}".getBytes(StandardCharsets.UTF_8));

        Commit commit =
                Commit.builder()
                        .parents(parents)
                        .tree(new TreeRef(snapshot, samples))
                        .author(author)
                        .timestamp(1_700_000_000L)
                        .message(message)
                        .auruVersion("0.1.0")
                        .formatVersion(1)
                        .build();

        assertEquals(commit.id(), project.putCommit(commit));
        return commit;
    }

    private static ProjectClient createProject(String handle, String name) {
        ProjectClient project = client.project(handle);
        project.putProfile(new ProjectProfile(name, "dawproject"));
        return project;
    }

    @Test
    void reportsTheProvidersProtocolAndCapabilities() {
        assertEquals("auru-pm-v1", client.health().protocol());
        assertEquals(Optional.of("java-sdk-test"), client.health().providerId());
        assertTrue(client.capabilities().projectListing());
    }

    @Test
    void acceptsAnEndpointWithATrailingSlash() {
        assertEquals(provider.endpoint, AuruClient.to(provider.endpoint + "/").transport(new JdkHttpTransport()).connect().endpoint());
    }

    @Test
    void refusesAnEndpointThatIsNotHttp() {
        AuruException error =
                assertThrows(
                        AuruException.class, () -> AuruClient.to("ftp://pm.example.com").transport(new JdkHttpTransport()).connect());
        assertEquals(ErrorCode.BAD_REQUEST, error.code());
        assertTrue(error.getMessage().contains("http or https"));
    }

    @Test
    void reportsAnUnreachableEndpoint() {
        AuruException error =
                assertThrows(AuruException.class, () -> AuruClient.to("http://127.0.0.1:1").transport(new JdkHttpTransport()).connect());
        assertTrue(error.getMessage().contains("cannot reach"));
    }

    @Test
    void publishesAVersionAndReadsItBack() throws Exception {
        ProjectClient project = createProject("life-cycle", "Life Cycle");
        assertTrue(project.head().isEmpty());

        Commit commit = publish(project, "first take", List.of());
        project.advanceHead(Optional.empty(), commit.id());
        assertEquals(Optional.of(commit.id()), project.head());

        Commit fetched = project.getCommit(commit.id());
        assertEquals("first take", fetched.message());
        // Round-tripping through the provider must not disturb identity.
        assertTrue(fetched.verifyId());
        assertEquals(commit.id(), fetched.id());

        assertEquals(List.of("first take"), project.history().stream().map(CommitSummary::message).toList());
    }

    @Test
    void storesAProfileAndListsTheProject() throws Exception {
        ProjectClient project = createProject("catalogued", "placeholder");
        Commit commit = publish(project, "catalogued", List.of());
        project.advanceHead(Optional.empty(), commit.id());
        project.putProfile(
                new ProjectProfile(
                        "Night Drive",
                        "dawproject",
                        Optional.of("Drum & Bass, Jungle"),
                        List.of("wip"),
                        Optional.empty()));

        ProviderProject listed =
                client.listProjects().stream()
                        .filter(candidate -> candidate.handle().equals("catalogued"))
                        .findFirst()
                        .orElseThrow();
        assertEquals("Night Drive", listed.profile().orElseThrow().displayName());
        assertEquals(List.of("wip"), listed.profile().orElseThrow().tags());
    }

    @Test
    void pagesHistoryNewestFirst() throws Exception {
        ProjectClient project = createProject("paged", "Paged");
        Optional<ContentHash> parent = Optional.empty();
        ContentHash newest = null;

        for (String message : List.of("one", "two", "three")) {
            Commit commit =
                    publish(project, message, parent.map(List::of).orElse(List.of()));
            project.advanceHead(parent, commit.id());
            parent = Optional.of(commit.id());
            newest = commit.id();
        }

        assertEquals(
                List.of("three", "two", "one"),
                project.history().stream().map(CommitSummary::message).toList());
        assertEquals(2, project.history(2).size());
        assertEquals(
                List.of("two", "one"),
                project.history(0, Optional.of(newest)).stream().map(CommitSummary::message).toList());
    }

    @Test
    void reportsTheActualHeadWhenTheCallersIsStale() throws Exception {
        ProjectClient project = createProject("racing", "Racing");

        Commit first = publish(project, "first", List.of());
        project.advanceHead(Optional.empty(), first.id());
        Commit second = publish(project, "second", List.of(first.id()));
        project.advanceHead(Optional.of(first.id()), second.id());

        // A client that still believes HEAD is `first` loses the race, and has
        // to learn what it lost to without another round trip.
        Commit stale = publish(project, "stale", List.of(first.id()));
        HeadConflictException conflict =
                assertThrows(
                        HeadConflictException.class,
                        () -> project.advanceHead(Optional.of(first.id()), stale.id()));

        assertEquals(Optional.of(second.id()), conflict.current());
        assertEquals(ErrorCode.HEAD_CONFLICT, conflict.code());
        assertFalse(conflict.retryable());
    }

    @Test
    void verifiesADownloadAgainstItsOwnName() {
        ProjectClient project = createProject("blobs", "Blobs");
        byte[] audio = "kick.wav pretending to be audio".getBytes(StandardCharsets.UTF_8);

        ContentHash hash = ContentHash.of(audio);
        assertEquals(List.of(false), project.hasBlobs(List.of(hash)));
        assertEquals(hash, project.putBlob(audio));
        assertEquals(List.of(true), project.hasBlobs(List.of(hash)));
        assertArrayEquals(audio, project.getBlob(hash));

        // Re-uploading is not an error.
        assertEquals(hash, project.putBlob(audio));
    }

    @Test
    void readsAProjectSummaryWithoutFetchingTheSnapshot() throws Exception {
        ProjectClient project = createProject("summarized", "Summarized");

        // A summary blob as a writer with a normalizer would have produced it.
        byte[] summary =
                ("{\"schema\":1,\"format\":\"dawproject\",\"dawproject\":{\"title\":\"Night Drive\"}}")
                        .getBytes(StandardCharsets.UTF_8);
        ContentHash summaryHash = project.putBlob(summary);
        ContentHash snapshot = project.putBlob(fixture());
        ContentHash samples = project.putBlob("{\"entries\":[]}".getBytes(StandardCharsets.UTF_8));

        Commit commit =
                Commit.builder()
                        .tree(new TreeRef(snapshot, samples))
                        .author(author)
                        .timestamp(1_700_000_000L)
                        .message("with a summary")
                        .auruVersion("0.1.0")
                        .formatVersion(1)
                        .metadata(summaryHash)
                        .build();
        project.putCommit(commit);

        ProjectInfo info = project.projectInfo(commit).orElseThrow();
        assertEquals("dawproject", info.format());
        assertEquals(1, info.schema());
        assertEquals(Optional.of("Night Drive"), info.text("title"));
        assertTrue(summary.length < fixture().length * 4, "the summary should be the cheap read");
    }

    @Test
    void refusesACommitWhoseIdDoesNotMatchItsContent() throws Exception {
        // The builder cannot produce one, so this goes through the wire type
        // directly — which is what a hostile or broken client would do.
        ProjectClient project = createProject("tampered", "Tampered");
        ContentHash snapshot = project.putBlob(fixture());
        ContentHash samples = project.putBlob("{\"entries\":[]}".getBytes(StandardCharsets.UTF_8));

        Json forged =
                Json.object()
                        .put("id", "blake3:" + "a".repeat(64))
                        .put("parents", Json.array(List.of()))
                        .put(
                                "tree",
                                Json.object()
                                        .put("snapshot", snapshot.toString())
                                        .put("samples", samples.toString())
                                        .build())
                        .put("author", author.toJson())
                        .put("timestamp", 1_700_000_000L)
                        .put("message", "an id I did not compute")
                        .put("description", "")
                        .put("auru_version", "0.1.0")
                        .put("format_version", 1)
                        .build();

        Commit forgedCommit = Commit.fromJson(forged);
        assertFalse(forgedCommit.verifyId(), "the fixture should not verify");

        AuruException error =
                assertThrows(AuruException.class, () -> project.putCommit(forgedCommit));
        assertEquals(ErrorCode.BAD_REQUEST, error.code());
    }

    @Test
    void reportsAMissingCommitAsNotFound() {
        AuruException error =
                assertThrows(
                        AuruException.class,
                        () ->
                                client
                                        .project("life-cycle")
                                        .getCommit(ContentHash.parse("blake3:" + "b".repeat(64))));
        assertEquals(ErrorCode.NOT_FOUND, error.code());
    }

    @Test
    void rejectsAnEmptyProjectHandleBeforeSendingAnything() {
        assertThrows(AuruException.class, () -> client.project(""));
    }

    @Test
    void prunesHistoryWhenTheProviderSupportsIt() throws Exception {
        assertTrue(client.capabilities().historyRetention());
        ProjectClient project = createProject("pruned", "Pruned");

        Optional<ContentHash> parent = Optional.empty();
        for (String message : List.of("one", "two", "three", "four")) {
            Commit commit = publish(project, message, parent.map(List::of).orElse(List.of()));
            project.advanceHead(parent, commit.id());
            parent = Optional.of(commit.id());
        }
        assertEquals(4, project.history().size());

        Retention.Report report =
                project.pruneHistory(new Retention.Request(new Retention.Rule.Latest(2)));
        assertEquals(2, report.versionsRemoved());
        assertEquals(
                List.of("four", "three"),
                project.history().stream().map(CommitSummary::message).toList());
    }
}

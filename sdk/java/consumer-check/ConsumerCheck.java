import java.io.IOException;
import java.net.ServerSocket;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Instant;
import java.util.List;
import java.util.Optional;
import studio.auru.pm.AuruClient;
import studio.auru.pm.AuthorIdentity;
import studio.auru.pm.Commit;
import studio.auru.pm.CommitSummary;
import studio.auru.pm.ContentHash;
import studio.auru.pm.HeadConflictException;
import studio.auru.pm.JdkHttpTransport;
import studio.auru.pm.ProjectClient;
import studio.auru.pm.ProjectInfo;
import studio.auru.pm.ProjectProfile;
import studio.auru.pm.Protocol;
import studio.auru.pm.TreeRef;

/**
 * What a consumer actually writes, run with only auru-pm.jar on the classpath.
 *
 * <p>That classpath is the point. The library claims zero runtime dependencies, and the only way
 * to know is to give it nothing else and see whether it works.
 */
public final class ConsumerCheck {

    public static void main(String[] args) throws Exception {
        Path repoRoot = Path.of(args[0]);
        Process server = null;
        Path directory = Files.createTempDirectory("auru-consumer-");

        try {
            int port;
            try (ServerSocket probe = new ServerSocket(0)) {
                port = probe.getLocalPort();
            }

            Path config = directory.resolve("server.toml");
            Files.writeString(
                    config,
                    String.join(
                            "\n",
                            "version = 1",
                            "provider_id = \"consumer-check\"",
                            "listen = \"127.0.0.1:" + port + "\"",
                            "data_dir = \"" + directory.resolve("data") + "\"",
                            "requests_per_minute = 100000",
                            "",
                            "[authentication]",
                            "mode = \"none\"",
                            ""));

            server =
                    new ProcessBuilder(
                                    repoRoot.resolve("target/debug/auru-pm-server").toString(),
                                    "--config",
                                    config.toString())
                            .redirectOutput(ProcessBuilder.Redirect.DISCARD)
                            .redirectError(ProcessBuilder.Redirect.DISCARD)
                            .start();

            String endpoint = "http://127.0.0.1:" + port;
            waitFor(endpoint);

            // ── the actual usage ────────────────────────────────────────────
            System.out.println("client protocol:  " + Protocol.VERSION);

            AuruClient client = AuruClient.to(endpoint).transport(new JdkHttpTransport()).connect();
            System.out.println(
                    "connected to:     "
                            + client.health().providerId().orElse("?")
                            + " at "
                            + client.endpoint());

            AuthorIdentity author = client.me().asAuthor();
            ProjectClient project = client.project("demo/night-drive");
            project.putProfile(new ProjectProfile("Night Drive", "dawproject"));

            byte[] snapshot =
                    Files.readAllBytes(
                            repoRoot.resolve(
                                    "crates/auru-pm-kernel/tests/fixtures/interchange/oracle-midi.dawproject"));
            byte[] summary =
                    ("{\"schema\":1,\"format\":\"dawproject\",\"dawproject\":{\"title\":\"Night Drive\"}}")
                            .getBytes(StandardCharsets.UTF_8);

            ContentHash snapshotHash = project.putBlob(snapshot);
            ContentHash samplesHash =
                    project.putBlob("{\"entries\":[]}".getBytes(StandardCharsets.UTF_8));
            ContentHash summaryHash = project.putBlob(summary);

            Commit commit =
                    Commit.builder()
                            .tree(new TreeRef(snapshotHash, samplesHash))
                            .author(author)
                            .timestamp(Instant.now().getEpochSecond())
                            .message("first take")
                            .auruVersion("0.1.0")
                            .formatVersion(1)
                            .metadata(summaryHash)
                            .build();

            project.putCommit(commit);
            project.advanceHead(Optional.empty(), commit.id());
            System.out.println("published:        " + commit.id());

            // The dashboard read: the summary blob, never the snapshot.
            Commit head = project.getCommit(project.head().orElseThrow());
            ProjectInfo info = project.projectInfo(head).orElseThrow();
            System.out.println(
                    "ProjectInfo:      "
                            + summary.length
                            + " bytes (snapshot is "
                            + snapshot.length
                            + ")");
            System.out.println("title:            " + info.text("title").orElseThrow());

            List<CommitSummary> history = project.history(10);
            System.out.println("history:          " + history.size() + " version(s)");

            // Losing a compare-and-swap.
            try {
                project.advanceHead(Optional.empty(), commit.id());
                System.out.println("ERROR: a stale compare-and-swap was accepted");
                System.exit(1);
            } catch (HeadConflictException conflict) {
                System.out.println(
                        "CAS conflict:     provider HEAD is "
                                + conflict.current().orElseThrow().toString().substring(0, 20)
                                + "...");
            }

            System.out.println();
            System.out.println("consumer flow complete, with only auru-pm.jar on the classpath");
        } finally {
            if (server != null) {
                server.destroy();
            }
            try (var entries = Files.walk(directory)) {
                entries.sorted(java.util.Comparator.reverseOrder()).forEach(p -> p.toFile().delete());
            }
        }
    }

    private static void waitFor(String endpoint) throws IOException, InterruptedException {
        HttpClient http = HttpClient.newHttpClient();
        for (int attempt = 0; attempt < 200; attempt++) {
            try {
                HttpResponse<String> response =
                        http.send(
                                HttpRequest.newBuilder(URI.create(endpoint + "/v1/health")).build(),
                                HttpResponse.BodyHandlers.ofString());
                if (response.statusCode() == 200) {
                    return;
                }
            } catch (IOException notListeningYet) {
                // keep waiting
            }
            Thread.sleep(50);
        }
        throw new IOException("the provider did not start on " + endpoint);
    }
}

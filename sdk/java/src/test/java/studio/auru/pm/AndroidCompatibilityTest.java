package studio.auru.pm;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.stream.Stream;
import org.junit.jupiter.api.AfterAll;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.TestInstance;

/**
 * The library working through a transport Android can actually run.
 *
 * <p>{@code java.net.http} is not part of the Android SDK, so a library that reached for it would
 * fail at class load on a phone. Everything below {@link Transport} avoids it, and this test proves
 * that by driving a full publish through {@link HttpURLConnection} — which Android does ship —
 * without {@link JdkHttpTransport} ever being loaded.
 */
@TestInstance(TestInstance.Lifecycle.PER_CLASS)
class AndroidCompatibilityTest {

    /** What an Android app would write, near enough. A real one would wrap OkHttp. */
    private static final class HttpUrlConnectionTransport implements Transport {
        @Override
        public Response send(Request request) throws IOException {
            HttpURLConnection connection = (HttpURLConnection) new URL(request.url()).openConnection();
            connection.setRequestMethod(request.method());
            for (Map.Entry<String, String> header : request.headers()) {
                connection.setRequestProperty(header.getKey(), header.getValue());
            }
            if (request.body() != null) {
                connection.setDoOutput(true);
                connection.getOutputStream().write(request.body());
            }

            int status = connection.getResponseCode();
            InputStream stream =
                    status / 100 == 2 ? connection.getInputStream() : connection.getErrorStream();

            byte[] body = new byte[0];
            if (stream != null) {
                ByteArrayOutputStream buffer = new ByteArrayOutputStream();
                byte[] chunk = new byte[8192];
                int read;
                while ((read = stream.read(chunk)) > 0) {
                    buffer.write(chunk, 0, read);
                }
                body = buffer.toByteArray();
            }

            List<Map.Entry<String, String>> headers = new ArrayList<>();
            connection
                    .getHeaderFields()
                    .forEach(
                            (name, values) -> {
                                if (name != null) {
                                    for (String value : values) {
                                        headers.add(Map.entry(name, value));
                                    }
                                }
                            });
            return new Response(status, headers, body);
        }
    }

    private static TestProvider provider;
    private static AuruClient client;

    @BeforeAll
    static void startProvider() throws Exception {
        provider = TestProvider.start();
        client = AuruClient.to(provider.endpoint).transport(new HttpUrlConnectionTransport()).connect();
    }

    @AfterAll
    static void stopProvider() throws Exception {
        if (provider != null) {
            provider.close();
        }
    }

    @Test
    void theClientCoreTouchesNothingAndroidLacks() throws Exception {
        // A phone would fail at class load, not at call time, so the check that
        // matters is which classes the core actually references.
        try (Stream<Path> sources =
                Files.walk(Path.of("src/main/java/studio/auru/pm"))) {
            List<String> offenders =
                    sources.filter(path -> path.toString().endsWith(".java"))
                            .filter(path -> !path.getFileName().toString().equals("JdkHttpTransport.java"))
                            .filter(
                                    path -> {
                                        try {
                                            return Files.readString(path)
                                                    .lines()
                                                    .anyMatch(line -> line.startsWith("import java.net.http"));
                                        } catch (IOException error) {
                                            throw new IllegalStateException(error);
                                        }
                                    })
                            .map(Path::toString)
                            .toList();
            assertTrue(
                    offenders.isEmpty(),
                    "these import java.net.http, which Android does not ship: " + offenders);
        }
    }

    @Test
    void publishesAVersionThroughAnAndroidCompatibleTransport() throws Exception {
        AuthorIdentity author = client.me().asAuthor();
        ProjectClient project = client.project("android");
        project.putProfile(new ProjectProfile("From A Phone", "dawproject"));

        ContentHash snapshot =
                project.putBlob("{\"auru_pm_snapshot\":1}".getBytes(StandardCharsets.UTF_8));
        ContentHash samples = project.putBlob("{\"entries\":[]}".getBytes(StandardCharsets.UTF_8));

        Commit commit =
                Commit.builder()
                        .tree(new TreeRef(snapshot, samples))
                        .author(author)
                        .timestamp(1_700_000_000L)
                        .message("published from a phone")
                        .auruVersion("0.1.0")
                        .formatVersion(1)
                        .build();

        assertEquals(commit.id(), project.putCommit(commit));
        project.advanceHead(Optional.empty(), commit.id());

        assertEquals(Optional.of(commit.id()), project.head());
        assertEquals(
                List.of("published from a phone"),
                project.history().stream().map(CommitSummary::message).toList());
        // Content addressing still holds through a different transport.
        assertTrue(project.getCommit(commit.id()).verifyId());
    }

    @Test
    void surfacesProviderErrorsThroughTheSameTransport() {
        AuruException error =
                org.junit.jupiter.api.Assertions.assertThrows(
                        AuruException.class,
                        () ->
                                client
                                        .project("android")
                                        .getCommit(ContentHash.parse("blake3:" + "e".repeat(64))));
        assertEquals(ErrorCode.NOT_FOUND, error.code());
    }
}

package studio.auru.pm;

import java.io.IOException;
import java.net.ServerSocket;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Comparator;
import java.util.stream.Stream;

/**
 * A real {@code auru-pm-server}, for the end-to-end tests.
 *
 * <p>Mocked HTTP would only prove this client agrees with the test's idea of the protocol. Running
 * the reference implementation is the only way to find out whether the two interoperate.
 */
final class TestProvider implements AutoCloseable {

    private final Process process;
    private final Path directory;
    final String endpoint;

    private TestProvider(Process process, Path directory, String endpoint) {
        this.process = process;
        this.directory = directory;
        this.endpoint = endpoint;
    }

    static Path repoRoot() {
        return Path.of(System.getProperty("auru.repoRoot"));
    }

    static TestProvider start() throws IOException, InterruptedException {
        Path repoRoot = repoRoot();
        new ProcessBuilder("cargo", "build", "-p", "auru-pm-server", "--locked")
                .directory(repoRoot.toFile())
                .inheritIO()
                .start()
                .waitFor();

        int port;
        try (ServerSocket probe = new ServerSocket(0)) {
            port = probe.getLocalPort();
        }

        Path directory = Files.createTempDirectory("auru-pm-java-");
        Path config = directory.resolve("server.toml");
        Files.writeString(
                config,
                String.join(
                        "\n",
                        "version = 1",
                        "provider_id = \"java-sdk-test\"",
                        "listen = \"127.0.0.1:" + port + "\"",
                        "data_dir = \"" + directory.resolve("data").toString().replace("\\", "\\\\") + "\"",
                        "requests_per_minute = 100000",
                        "",
                        "[authentication]",
                        "mode = \"none\"",
                        ""));

        Process process =
                new ProcessBuilder(
                                repoRoot.resolve("target/debug/auru-pm-server").toString(),
                                "--config",
                                config.toString())
                        .directory(directory.toFile())
                        .redirectOutput(ProcessBuilder.Redirect.DISCARD)
                        .redirectError(ProcessBuilder.Redirect.DISCARD)
                        .start();

        String endpoint = "http://127.0.0.1:" + port;
        HttpClient http = HttpClient.newHttpClient();
        for (int attempt = 0; attempt < 200; attempt++) {
            try {
                HttpResponse<String> response =
                        http.send(
                                HttpRequest.newBuilder(URI.create(endpoint + "/v1/health")).build(),
                                HttpResponse.BodyHandlers.ofString());
                if (response.statusCode() == 200) {
                    return new TestProvider(process, directory, endpoint);
                }
            } catch (IOException notListeningYet) {
                // keep waiting
            }
            Thread.sleep(50);
        }

        process.destroyForcibly();
        throw new IOException("the provider did not start on " + endpoint);
    }

    @Override
    public void close() throws IOException {
        process.destroy();
        try (Stream<Path> entries = Files.walk(directory)) {
            entries.sorted(Comparator.reverseOrder()).forEach(path -> path.toFile().delete());
        }
    }
}

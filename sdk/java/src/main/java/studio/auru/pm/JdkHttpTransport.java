package studio.auru.pm;

import java.io.IOException;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;

/**
 * A {@link Transport} over the JDK's {@code HttpClient}.
 *
 * <p>Deliberately its own class. {@code java.net.http} is not part of the Android SDK, and a phone
 * that never references this class never loads it — which is what lets the rest of the library run
 * there unchanged. On Android, implement {@link Transport} over OkHttp or whatever the app already
 * uses.
 */
public final class JdkHttpTransport implements Transport {

    private final HttpClient http;
    private final Duration requestTimeout;

    public JdkHttpTransport() {
        this(Duration.ofSeconds(10), Duration.ofSeconds(60));
    }

    /**
     * @param connectTimeout how long to wait for a connection
     * @param requestTimeout how long one request may take. Generous by default because a blob can
     *     be large; lower it for an interactive path where a stall is worse than a failure.
     */
    public JdkHttpTransport(Duration connectTimeout, Duration requestTimeout) {
        this(HttpClient.newBuilder().connectTimeout(connectTimeout).build(), requestTimeout);
    }

    /** Wrap a configured client, for a proxy, a custom TLS context, or a test. */
    public JdkHttpTransport(HttpClient http, Duration requestTimeout) {
        this.http = http;
        this.requestTimeout = requestTimeout;
    }

    @Override
    public Response send(Request request) throws IOException {
        HttpRequest.Builder builder =
                HttpRequest.newBuilder(URI.create(request.url())).timeout(requestTimeout);
        for (Map.Entry<String, String> header : request.headers()) {
            builder.header(header.getKey(), header.getValue());
        }
        builder.method(
                request.method(),
                request.body() == null
                        ? HttpRequest.BodyPublishers.noBody()
                        : HttpRequest.BodyPublishers.ofByteArray(request.body()));

        HttpResponse<byte[]> response;
        try {
            response = http.send(builder.build(), HttpResponse.BodyHandlers.ofByteArray());
        } catch (InterruptedException cause) {
            // Restore the flag rather than swallowing it: whoever interrupted
            // this thread is still waiting for it to notice.
            Thread.currentThread().interrupt();
            throw new IOException(request.method() + " " + request.url() + " was interrupted", cause);
        }

        List<Map.Entry<String, String>> headers = new ArrayList<>();
        response.headers()
                .map()
                .forEach(
                        (name, values) -> {
                            for (String value : values) {
                                headers.add(Map.entry(name, value));
                            }
                        });
        return new Response(response.statusCode(), headers, response.body());
    }
}

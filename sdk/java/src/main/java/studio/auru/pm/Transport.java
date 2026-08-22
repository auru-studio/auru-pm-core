package studio.auru.pm;

import java.io.IOException;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Optional;

/**
 * How this client reaches a provider.
 *
 * <p>An interface rather than a fixed HTTP stack. The JDK's {@code java.net.http.HttpClient} is not
 * part of the Android SDK, so a library that used it directly would fail at class load on a phone —
 * and most applications that would use this already have an HTTP stack whose TLS configuration,
 * proxy settings and certificates are already being kept current. Forcing a second one is the wrong
 * trade for saving a caller twenty lines.
 *
 * <p>{@link JdkHttpTransport} covers the JVM. On Android, wrap OkHttp or whatever the app already
 * uses; the interface is four methods' worth of work.
 *
 * <p>Implementations must be safe to call from several threads if the client is shared.
 */
public interface Transport {

    /** One HTTP request. */
    final class Request {
        private final String method;
        private final String url;
        private final List<Map.Entry<String, String>> headers;
        private final byte[] body;

        Request(String method, String url, List<Map.Entry<String, String>> headers, byte[] body) {
            this.method = method;
            this.url = url;
            this.headers = List.copyOf(headers);
            this.body = body;
        }

        public String method() {
            return method;
        }

        public String url() {
            return url;
        }

        public List<Map.Entry<String, String>> headers() {
            return headers;
        }

        /** The request body, or {@code null} when there is none. */
        public byte[] body() {
            return body;
        }
    }

    /** One HTTP response. */
    final class Response {
        private final int status;
        private final List<Map.Entry<String, String>> headers;
        private final byte[] body;

        public Response(int status, List<Map.Entry<String, String>> headers, byte[] body) {
            this.status = status;
            this.headers = List.copyOf(headers);
            this.body = body == null ? new byte[0] : body;
        }

        public int status() {
            return status;
        }

        public byte[] body() {
            return body;
        }

        /** A header by name, matched case-insensitively as HTTP requires. */
        public Optional<String> header(String name) {
            String wanted = name.toLowerCase(Locale.ROOT);
            for (Map.Entry<String, String> entry : headers) {
                if (entry.getKey().toLowerCase(Locale.ROOT).equals(wanted)) {
                    return Optional.of(entry.getValue());
                }
            }
            return Optional.empty();
        }

        /** Convenience for building a response from a header map. */
        public static Response of(int status, Map<String, String> headers, byte[] body) {
            return new Response(status, new ArrayList<>(headers.entrySet()), body);
        }
    }

    /**
     * Perform one request.
     *
     * <p>Throw only when the request could not be completed at all — DNS, connection, TLS, timeout.
     * A non-2xx response is a {@link Response}, not an exception: interpreting a provider's status
     * is this library's job, and a transport that guessed would have to be corrected here anyway.
     */
    Response send(Request request) throws IOException;
}

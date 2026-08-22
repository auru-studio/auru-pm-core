package studio.auru.pm;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.sun.net.httpserver.HttpExchange;
import com.sun.net.httpserver.HttpServer;
import java.io.IOException;
import java.net.InetSocketAddress;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.concurrent.atomic.AtomicReference;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;

/**
 * How this client behaves when a provider misbehaves.
 *
 * <p>Providers are written by third parties, so the interesting cases are the ones outside the
 * table — an invented code, or a proxy answering with an HTML error page instead of JSON. Those are
 * ordinary, not hypothetical, which is why they are exercised against a stub that can produce them
 * rather than against the well-behaved reference server.
 */
class ErrorMappingTest {

    private static final String HEALTH =
            "{\"protocol\":\"auru-pm-v1\",\"provider_id\":\"stub\",\"capabilities\":"
                    + "{\"project_listing\":true,\"members\":false,\"permissions\":false,"
                    + "\"branches\":false,\"server_side_merge\":false,\"history_retention\":false,"
                    + "\"project_scoped_blobs\":true,\"auth_methods\":[\"none\"]}}";

    private HttpServer server;
    private final AtomicReference<String> lastAuthorization = new AtomicReference<>();

    /** A provider that answers health normally and everything else as told. */
    private AuruClient stub(int status, String body, Map<String, String> headers) throws IOException {
        server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        server.createContext(
                "/",
                (HttpExchange exchange) -> {
                    lastAuthorization.set(exchange.getRequestHeaders().getFirst("Authorization"));
                    boolean health = exchange.getRequestURI().getPath().equals("/v1/health");
                    byte[] payload = (health ? HEALTH : body).getBytes(StandardCharsets.UTF_8);
                    headers.forEach((name, value) -> exchange.getResponseHeaders().add(name, value));
                    exchange.sendResponseHeaders(health ? 200 : status, payload.length);
                    exchange.getResponseBody().write(payload);
                    exchange.close();
                });
        server.start();
        return AuruClient.to("http://127.0.0.1:" + server.getAddress().getPort())
                .transport(new JdkHttpTransport())
                .connect();
    }

    @AfterEach
    void stop() {
        if (server != null) {
            server.stop(0);
        }
    }

    @Test
    void keepsTheCodeAndMessageAProviderSent() throws IOException {
        AuruClient client = stub(403, "{\"code\":\"forbidden\",\"message\":\"not your project\"}", Map.of());
        AuruException error = assertThrows(AuruException.class, client::me);
        assertEquals(ErrorCode.FORBIDDEN, error.code());
        assertEquals("not your project", error.getMessage());
        assertEquals(403, error.status().orElseThrow());
        assertFalse(error.retryable());
    }

    @Test
    void surfacesHeadConflictWithTheCurrentHead() throws IOException {
        String current = "blake3:" + "c".repeat(64);
        AuruClient client =
                stub(409, "{\"code\":\"head_conflict\",\"current\":\"" + current + "\"}", Map.of());

        HeadConflictException conflict =
                assertThrows(
                        HeadConflictException.class,
                        () ->
                                client
                                        .project("p")
                                        .advanceHead(
                                                Optional.empty(),
                                                ContentHash.parse("blake3:" + "d".repeat(64))));
        assertEquals(Optional.of(ContentHash.parse(current)), conflict.current());
    }

    @Test
    void readsANullCurrentHeadOnAnEmptyProject() throws IOException {
        AuruClient client = stub(409, "{\"code\":\"head_conflict\",\"current\":null}", Map.of());
        HeadConflictException conflict =
                assertThrows(
                        HeadConflictException.class,
                        () ->
                                client
                                        .project("p")
                                        .advanceHead(
                                                Optional.of(ContentHash.parse("blake3:" + "e".repeat(64))),
                                                ContentHash.parse("blake3:" + "f".repeat(64))));
        assertTrue(conflict.current().isEmpty());
    }

    @Test
    void carriesRetryAfterOnARateLimitAndMarksItRetryable() throws IOException {
        AuruClient client =
                stub(
                        429,
                        "{\"code\":\"rate_limited\",\"message\":\"slow down\"}",
                        Map.of("Retry-After", "60"));
        AuruException error = assertThrows(AuruException.class, client::me);
        assertEquals(ErrorCode.RATE_LIMITED, error.code());
        assertEquals(Duration.ofSeconds(60), error.retryAfter().orElseThrow());
        assertTrue(error.retryable());
    }

    @Test
    void treatsAnUnreachableIdentityProviderAsRetryableNotAsABadToken() throws IOException {
        // Prompting someone to sign in again here would be wrong: the token was
        // never judged.
        AuruClient client =
                stub(503, "{\"code\":\"authentication_unavailable\",\"message\":\"idp down\"}", Map.of());
        AuruException error = assertThrows(AuruException.class, client::me);
        assertEquals(ErrorCode.AUTHENTICATION_UNAVAILABLE, error.code());
        assertTrue(error.retryable());
    }

    @Test
    void fallsBackToTheStatusWhenAProviderInventsACode() throws IOException {
        AuruClient client = stub(404, "{\"code\":\"teapot\",\"message\":\"unusual\"}", Map.of());
        AuruException error = assertThrows(AuruException.class, client::me);
        assertEquals(ErrorCode.NOT_FOUND, error.code());
        assertEquals("unusual", error.getMessage());
    }

    @Test
    void survivesAProxyAnsweringWithHtml() throws IOException {
        AuruClient client = stub(502, "<html>502 Bad Gateway</html>", Map.of());
        AuruException error = assertThrows(AuruException.class, client::me);
        assertEquals(ErrorCode.INTERNAL, error.code());
        assertTrue(error.retryable());
        assertTrue(error.getMessage().contains("502"));
    }

    @Test
    void refusesAProviderSpeakingADifferentWireVersion() throws IOException {
        server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        server.createContext(
                "/",
                exchange -> {
                    byte[] payload =
                            HEALTH.replace("auru-pm-v1", "auru-pm-v2").getBytes(StandardCharsets.UTF_8);
                    exchange.sendResponseHeaders(200, payload.length);
                    exchange.getResponseBody().write(payload);
                    exchange.close();
                });
        server.start();

        AuruException error =
                assertThrows(
                        AuruException.class,
                        () ->
                                AuruClient.to("http://127.0.0.1:" + server.getAddress().getPort())
                                        .transport(new JdkHttpTransport())
                                        .connect());
        assertEquals(ErrorCode.UNSUPPORTED, error.code());
        assertTrue(error.getMessage().contains("auru-pm-v2"));
    }

    @Test
    void refusesAnUnadvertisedCapabilityBeforeARoundTrip() throws IOException {
        AuruClient client = stub(200, "{}", Map.of());
        assertFalse(client.capabilities().historyRetention());

        AuruException error =
                assertThrows(
                        AuruException.class,
                        () ->
                                client
                                        .project("p")
                                        .pruneHistory(new Retention.Request(new Retention.Rule.Latest(10))));
        assertEquals(ErrorCode.UNSUPPORTED, error.code());
        // Nothing was sent: only the health request ever reached the stub.
        assertTrue(error.getMessage().contains("history retention"));
    }

    @Test
    void sendsTheBearerTokenAndNeverExposesIt() throws IOException {
        AuruClient client = stub(200, "{\"commit_id\":null}", Map.of());
        client.accessToken("secret-token");
        client.project("p").head();
        assertEquals("Bearer secret-token", lastAuthorization.get());
        assertTrue(client.authenticated());

        client.accessToken(null);
        client.project("p").head();
        assertEquals(null, lastAuthorization.get());
        assertFalse(client.authenticated());
    }

    @Test
    void verifiesADownloadedBlobAndRejectsTamperedBytes() throws IOException {
        // The stub returns bytes that do not hash to the requested name.
        AuruClient client = stub(200, "not the bytes you asked for", Map.of());
        ContentHash expected = ContentHash.of("the real bytes".getBytes(StandardCharsets.UTF_8));

        AuruException error =
                assertThrows(AuruException.class, () -> client.project("p").getBlob(expected));
        assertTrue(error.getMessage().contains("does not hash to its own name"));
    }

    @Test
    void anEmptyBlobListIsNotARequest() throws IOException {
        AuruClient client = stub(500, "{\"code\":\"internal\"}", Map.of());
        // Asking about nothing must not reach the provider at all.
        assertEquals(List.of(), client.project("p").hasBlobs(List.of()));
    }
}

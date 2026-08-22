package studio.auru.pm;

import java.io.IOException;
import java.net.URI;
import java.net.URISyntaxException;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.Optional;
import java.util.OptionalInt;

/**
 * A connected {@code auru-pm-v1} provider.
 *
 * <p>The endpoint is always supplied by the caller. There is no default host here and no registry
 * lookup: anyone can run a provider, and which one to trust is the user's decision rather than this
 * library's.
 *
 * <pre>{@code
 * AuruClient client = AuruClient.to("https://pm.example.com")
 *     .transport(new JdkHttpTransport())
 *     .accessToken(token)
 *     .connect();
 *
 * for (CommitSummary version : client.project("user/night-drive").history(20)) {
 *     System.out.println(version.timestamp() + "  " + version.message());
 * }
 * }</pre>
 *
 * <p>A {@link Transport} is required rather than defaulted. The JDK's {@code HttpClient} is not
 * part of the Android SDK, and a library that reached for it implicitly would fail at class load on
 * a phone — so the choice is the caller's, and it is visible.
 *
 * <p>Instances are safe to share between threads: the only mutable state is the bearer token, and
 * it is written and read through a {@code volatile} field so a refresh on one thread is visible to
 * the rest.
 */
public final class AuruClient {

    private final String endpoint;
    private final ProviderHealth health;
    private final Transport transport;
    private volatile String accessToken;

    private AuruClient(String endpoint, ProviderHealth health, Builder builder) {
        this.endpoint = endpoint;
        this.health = health;
        this.transport = builder.transport;
        this.accessToken = builder.accessToken;
    }

    /** Start configuring a connection to {@code endpoint}. */
    public static Builder to(String endpoint) {
        return new Builder(endpoint);
    }

    /**
     * Read {@code /v1/health} and return a client bound to that provider.
     *
     * <p>Health is read first because the protocol version, the auth methods and the capability set
     * all have to be known before the first real call — one round trip up front is cheaper than
     * discovering an unsupported endpoint halfway through a push.
     *
     * @throws AuruException if the endpoint is unreachable, or speaks a different wire version
     */
    public static AuruClient connect(Builder builder) {
        Objects.requireNonNull(builder.transport, "a Transport is required; see JdkHttpTransport");
        String endpoint = normalizeEndpoint(builder.endpoint);

        Transport.Response response =
                send(
                        builder.transport,
                        new Transport.Request(
                                "GET",
                                endpoint + "/v1/health",
                                List.of(Map.entry("accept", "application/json")),
                                null),
                        "GET /v1/health");
        if (response.status() / 100 != 2) {
            throw failure(response);
        }

        ProviderHealth health =
                ProviderHealth.fromJson(
                        Json.parse(new String(response.body(), StandardCharsets.UTF_8)));
        if (!Protocol.VERSION.equals(health.protocol())) {
            throw new AuruException(
                    ErrorCode.UNSUPPORTED,
                    endpoint
                            + " speaks "
                            + health.protocol()
                            + "; this client speaks "
                            + Protocol.VERSION);
        }
        return new AuruClient(endpoint, health, builder);
    }

    /** What the provider said about itself. */
    public ProviderHealth health() {
        return health;
    }

    public Capabilities capabilities() {
        return health.capabilities();
    }

    /** The normalized endpoint this client talks to. */
    public String endpoint() {
        return endpoint;
    }

    /** Replace the bearer token, after a refresh for instance. */
    public void accessToken(String token) {
        this.accessToken = token;
    }

    /** Whether a token is held. Never exposes the token itself. */
    public boolean authenticated() {
        return accessToken != null;
    }

    /** The identity the provider derived from the current token. */
    public AuthenticatedIdentity me() {
        return AuthenticatedIdentity.fromJson(json("GET", "/v1/me", null));
    }

    /** Projects visible to this account, newest first. */
    public List<ProviderProject> listProjects() {
        require(capabilities().projectListing(), "project_listing", "listing projects");
        List<ProviderProject> projects = new ArrayList<>();
        for (Json project : json("GET", "/v1/projects", null).require("projects").elements()) {
            projects.add(ProviderProject.fromJson(project));
        }
        return projects;
    }

    /** Scope subsequent calls to one project handle. */
    public ProjectClient project(String handle) {
        if (handle == null || handle.isEmpty()) {
            throw new AuruException(ErrorCode.BAD_REQUEST, "a project handle must not be empty");
        }
        return new ProjectClient(this, handle);
    }

    // ── Internals ────────────────────────────────────────────────────────────

    void require(boolean advertised, String capability, String action) {
        if (!advertised) {
            throw new AuruException(
                    ErrorCode.UNSUPPORTED,
                    endpoint + " does not support " + action + " (capability " + capability + ")");
        }
    }

    Json json(String method, String path, Json body) {
        Transport.Response response = request(method, path, body, "application/json");
        byte[] payload = response.body();
        return payload.length == 0
                ? Json.object().build()
                : Json.parse(new String(payload, StandardCharsets.UTF_8));
    }

    Transport.Response request(String method, String path, Json body, String accept) {
        return exchange(method, path, accept, body == null ? null : "application/json",
                body == null ? null : body.toJsonText().getBytes(StandardCharsets.UTF_8));
    }

    Transport.Response requestBytes(String method, String path, byte[] body) {
        return exchange(method, path, "application/octet-stream",
                body == null ? null : "application/octet-stream", body);
    }

    private Transport.Response exchange(
            String method, String path, String accept, String contentType, byte[] body) {
        List<Map.Entry<String, String>> headers = new ArrayList<>();
        headers.add(Map.entry("accept", accept));

        String token = accessToken;
        if (token != null) {
            headers.add(Map.entry("authorization", "Bearer " + token));
        }
        if (contentType != null) {
            headers.add(Map.entry("content-type", contentType));
        }

        Transport.Response response =
                send(
                        transport,
                        new Transport.Request(method, endpoint + path, headers, body),
                        method + " " + path);
        if (response.status() / 100 != 2) {
            throw failure(response);
        }
        return response;
    }

    private static Transport.Response send(
            Transport transport, Transport.Request request, String what) {
        try {
            return transport.send(request);
        } catch (IOException cause) {
            throw new AuruException(
                    ErrorCode.INTERNAL, "cannot reach " + what + ": " + cause, cause);
        }
    }

    /**
     * Turn a non-2xx response into the right exception.
     *
     * <p>A provider that invents a code outside the table, or answers with something that is not
     * JSON at all, still has to produce a usable error here — a proxy returning an HTML 502 page is
     * the ordinary case, not a hypothetical.
     */
    private static AuruException failure(Transport.Response response) {
        Json body;
        try {
            body = Json.parse(new String(response.body(), StandardCharsets.UTF_8));
        } catch (RuntimeException notJson) {
            body = Json.object().build();
        }

        Optional<String> rawCode = body.optString("code");
        if (rawCode.filter("head_conflict"::equals).isPresent()) {
            return new HeadConflictException(body.optString("current").map(ContentHash::parse));
        }

        ErrorCode code =
                rawCode.flatMap(ErrorCode::fromWire)
                        .orElseGet(() -> ErrorCode.forStatus(response.status()));
        String message =
                body.optString("message")
                        .orElseGet(() -> "HTTP " + response.status() + " from the provider");

        Optional<Duration> retryAfter =
                response
                        .header("retry-after")
                        .filter(value -> value.matches("\\d+"))
                        .map(value -> Duration.ofSeconds(Long.parseLong(value)));

        return new AuruException(code, message, OptionalInt.of(response.status()), retryAfter, null);
    }

    private static String normalizeEndpoint(String endpoint) {
        URI parsed;
        try {
            parsed = new URI(endpoint);
        } catch (URISyntaxException cause) {
            throw new AuruException(
                    ErrorCode.BAD_REQUEST, "endpoint is not a URI: " + endpoint, cause);
        }
        String scheme = parsed.getScheme();
        if (!"http".equals(scheme) && !"https".equals(scheme)) {
            throw new AuruException(
                    ErrorCode.BAD_REQUEST,
                    "endpoint must be http or https, got " + (scheme == null ? "no scheme" : scheme));
        }
        if (parsed.getHost() == null) {
            throw new AuruException(ErrorCode.BAD_REQUEST, "endpoint must name a host: " + endpoint);
        }
        String path = parsed.getRawPath() == null ? "" : parsed.getRawPath().replaceAll("/+$", "");
        return parsed.getScheme() + "://" + parsed.getRawAuthority() + path;
    }

    /** Connection settings. */
    public static final class Builder {
        private final String endpoint;
        private Transport transport;
        private String accessToken;

        private Builder(String endpoint) {
            this.endpoint = endpoint;
        }

        /**
         * How to reach the provider. Required.
         *
         * <p>{@link JdkHttpTransport} on the JVM. On Android, wrap OkHttp or whatever the app
         * already uses — {@code java.net.http} is not available there.
         */
        public Builder transport(Transport transport) {
            this.transport = transport;
            return this;
        }

        /**
         * The bearer token.
         *
         * <p>Held in memory for the lifetime of the client and written nowhere else. Persisting one
         * is the caller's decision and should use a real secret store — an OS keychain, a secrets
         * manager — rather than a file beside the application.
         */
        public Builder accessToken(String token) {
            this.accessToken = token;
            return this;
        }

        /** Connect, reading {@code /v1/health}. */
        public AuruClient connect() {
            return AuruClient.connect(this);
        }
    }
}

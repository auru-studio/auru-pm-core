package studio.auru.pm;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.sun.net.httpserver.HttpExchange;
import com.sun.net.httpserver.HttpServer;
import java.io.IOException;
import java.net.InetSocketAddress;
import java.net.URI;
import java.net.URLDecoder;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.time.Duration;
import java.util.Base64;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.concurrent.atomic.AtomicReference;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;

/**
 * PKCE, discovery, and the token rules.
 *
 * <p>Most of what matters here is what the flow <em>refuses</em>. A client that accepts a discovery
 * document naming someone else's token endpoint, or that hands a refresh token to code with
 * nowhere safe to put it, fails in a way its users never see until it matters.
 */
class OAuthTest {

    private static final String ISSUER_PATH = "/identity";

    private HttpServer server;
    private String issuer;
    private final AtomicReference<String> lastTokenBody = new AtomicReference<>();

    private OAuthConfiguration.OAuthClient browserClient() {
        return new OAuthConfiguration.OAuthClient(
                OAuthConfiguration.Kind.BROWSER,
                "dashboard",
                "https://dashboard.example.com/oauth/callback",
                List.of("authorization_code_pkce"));
    }

    /** An identity provider that answers discovery and the token endpoint as told. */
    private void startIdentityProvider(String discoveryBody, int tokenStatus, String tokenBody)
            throws IOException {
        server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        issuer = "http://127.0.0.1:" + server.getAddress().getPort() + ISSUER_PATH;

        server.createContext(
                "/",
                (HttpExchange exchange) -> {
                    String path = exchange.getRequestURI().getPath();
                    byte[] payload;
                    int status;
                    if (path.endsWith("/.well-known/openid-configuration")) {
                        payload = discoveryBody.replace("{issuer}", issuer).getBytes(StandardCharsets.UTF_8);
                        status = discoveryBody.isEmpty() ? 404 : 200;
                    } else if (path.endsWith("/token")) {
                        lastTokenBody.set(
                                new String(exchange.getRequestBody().readAllBytes(), StandardCharsets.UTF_8));
                        payload = tokenBody.getBytes(StandardCharsets.UTF_8);
                        status = tokenStatus;
                    } else {
                        payload = new byte[0];
                        status = 404;
                    }
                    exchange.sendResponseHeaders(status, payload.length == 0 ? -1 : payload.length);
                    if (payload.length > 0) {
                        exchange.getResponseBody().write(payload);
                    }
                    exchange.close();
                });
        server.start();
    }

    private String discoveryDocument() {
        return "{\"issuer\":\"{issuer}\","
                + "\"authorization_endpoint\":\"{issuer}/authorize\","
                + "\"token_endpoint\":\"{issuer}/token\","
                + "\"code_challenge_methods_supported\":[\"S256\"]}";
    }

    private static final String TOKEN_RESPONSE =
            "{\"access_token\":\"an-access-token\",\"refresh_token\":\"a-refresh-token\","
                    + "\"token_type\":\"Bearer\",\"expires_in\":3600,\"scope\":\"openid\"}";

    @AfterEach
    void stop() {
        if (server != null) {
            server.stop(0);
        }
    }

    private static Map<String, String> query(String url) {
        Map<String, String> parameters = new HashMap<>();
        String raw = URI.create(url).getRawQuery();
        for (String pair : raw.split("&")) {
            int equals = pair.indexOf('=');
            parameters.put(
                    URLDecoder.decode(pair.substring(0, equals), StandardCharsets.UTF_8),
                    URLDecoder.decode(pair.substring(equals + 1), StandardCharsets.UTF_8));
        }
        return parameters;
    }

    // ── Choosing a client ────────────────────────────────────────────────────

    @Test
    void findsEachRegisteredClientKind() {
        OAuthConfiguration configuration =
                OAuthConfiguration.fromJson(
                        Json.parse(
                                "{\"issuer\":\"https://i.example.com\",\"audience\":\"auru-pm\","
                                    + "\"required_scope\":\"openid\",\"clients\":["
                                    + "{\"kind\":\"native\",\"client_id\":\"desktop\","
                                    + "\"redirect_uri\":\"http://127.0.0.1:43827/oauth/callback\","
                                    + "\"flows\":[\"authorization_code_pkce\"]},"
                                    + "{\"kind\":\"browser\",\"client_id\":\"dashboard\","
                                    + "\"redirect_uri\":\"https://dashboard.example.com/cb\","
                                    + "\"flows\":[\"authorization_code_pkce\"]},"
                                    + "{\"kind\":\"mobile\",\"client_id\":\"phones\","
                                    + "\"redirect_uri\":\"studio.auru.pm:/oauth/callback\","
                                    + "\"flows\":[\"authorization_code_pkce\","
                                    + "\"device_authorization\"]}]}"));

        assertEquals(
                "desktop",
                configuration.client(OAuthConfiguration.Kind.NATIVE).orElseThrow().clientId());
        assertEquals(
                "dashboard",
                configuration.client(OAuthConfiguration.Kind.BROWSER).orElseThrow().clientId());
        assertEquals(
                "phones",
                configuration.client(OAuthConfiguration.Kind.MOBILE).orElseThrow().clientId());
    }

    @Test
    void aClientKindNewerThanThisSdkIsKeptAsUnknownRatherThanMislabelled() {
        OAuthConfiguration configuration =
                OAuthConfiguration.fromJson(
                        Json.parse(
                                "{\"issuer\":\"https://i.example.com\",\"audience\":\"auru-pm\","
                                    + "\"required_scope\":\"openid\",\"clients\":["
                                    + "{\"kind\":\"holographic\",\"client_id\":\"future\","
                                    + "\"redirect_uri\":\"future:/cb\",\"flows\":[]}]}"));

        assertTrue(configuration.client(OAuthConfiguration.Kind.NATIVE).isEmpty());
        assertEquals(
                "future",
                configuration.client(OAuthConfiguration.Kind.UNKNOWN).orElseThrow().clientId());
    }

    @Test
    void readsThePreClientsSingularFieldsAsTheNativeClient() {
        // What every provider written before `clients` existed sends.
        OAuthConfiguration configuration =
                OAuthConfiguration.fromJson(
                        Json.parse(
                                "{\"issuer\":\"https://i.example.com\",\"audience\":\"auru-pm\","
                                    + "\"required_scope\":\"openid\",\"client_id\":\"desktop\","
                                    + "\"redirect_uri\":\"http://127.0.0.1:43827/oauth/callback\","
                                    + "\"flows\":[\"authorization_code_pkce\"]}"));

        assertEquals(
                "desktop",
                configuration.client(OAuthConfiguration.Kind.NATIVE).orElseThrow().clientId());
        assertTrue(configuration.client(OAuthConfiguration.Kind.BROWSER).isEmpty());
    }

    // ── Discovery ────────────────────────────────────────────────────────────

    @Test
    void acceptsADocumentWhoseIssuerMatchesExactly() throws IOException {
        startIdentityProvider(discoveryDocument(), 200, TOKEN_RESPONSE);
        OAuth.ServerMetadata metadata = OAuth.discover(issuer, new JdkHttpTransport());
        assertEquals(issuer + "/token", metadata.tokenEndpoint());
        assertEquals(List.of("S256"), metadata.codeChallengeMethodsSupported());
    }

    @Test
    void refusesADocumentClaimingADifferentIssuer() throws IOException {
        // A provider that could name someone else's issuer could send a user's
        // credentials there.
        startIdentityProvider(
                discoveryDocument().replace("\"issuer\":\"{issuer}\"", "\"issuer\":\"https://attacker.example.com\""),
                200,
                TOKEN_RESPONSE);
        AuruException error = assertThrows(AuruException.class, () -> OAuth.discover(issuer, new JdkHttpTransport()));
        assertTrue(error.getMessage().contains("claims issuer"), error.getMessage());
    }

    @Test
    void refusesADocumentMissingTheEndpointsPkceNeeds() throws IOException {
        startIdentityProvider("{\"issuer\":\"{issuer}\"}", 200, TOKEN_RESPONSE);
        AuruException error = assertThrows(AuruException.class, () -> OAuth.discover(issuer, new JdkHttpTransport()));
        assertTrue(error.getMessage().contains("omits the endpoints"), error.getMessage());
    }

    @Test
    void reportsAnIssuerItCannotReach() {
        AuruException error =
                assertThrows(AuruException.class, () -> OAuth.discover("http://127.0.0.1:1/identity", new JdkHttpTransport()));
        assertTrue(error.getMessage().contains("cannot discover"), error.getMessage());
    }

    // ── Beginning authorization ──────────────────────────────────────────────

    private OAuth.ServerMetadata metadata() {
        return new OAuth.ServerMetadata(
                "https://i.example.com",
                "https://i.example.com/authorize",
                "https://i.example.com/token",
                List.of("S256"));
    }

    @Test
    void buildsAnS256ChallengeAndAFreshState() throws Exception {
        OAuth.AuthorizationRequest request =
                OAuth.beginAuthorization(metadata(), browserClient(), "openid");

        Map<String, String> parameters = query(request.url());
        assertTrue(request.url().startsWith("https://i.example.com/authorize?"));
        assertEquals("code", parameters.get("response_type"));
        assertEquals("dashboard", parameters.get("client_id"));
        assertEquals("https://dashboard.example.com/oauth/callback", parameters.get("redirect_uri"));
        assertEquals("S256", parameters.get("code_challenge_method"));
        assertEquals(request.state(), parameters.get("state"));

        // The challenge must actually be SHA-256 of the verifier, not a copy.
        String expected =
                Base64.getUrlEncoder()
                        .withoutPadding()
                        .encodeToString(
                                MessageDigest.getInstance("SHA-256")
                                        .digest(request.codeVerifier().getBytes(StandardCharsets.US_ASCII)));
        assertEquals(expected, parameters.get("code_challenge"));
        assertNotEquals(request.codeVerifier(), parameters.get("code_challenge"));
    }

    @Test
    void givesEveryRequestItsOwnVerifierAndState() {
        OAuth.AuthorizationRequest first =
                OAuth.beginAuthorization(metadata(), browserClient(), "openid");
        OAuth.AuthorizationRequest second =
                OAuth.beginAuthorization(metadata(), browserClient(), "openid");
        assertNotEquals(first.codeVerifier(), second.codeVerifier());
        assertNotEquals(first.state(), second.state());
    }

    @Test
    void refusesAProviderThatDoesNotOfferS256() {
        OAuth.ServerMetadata plainOnly =
                new OAuth.ServerMetadata(
                        "https://i.example.com",
                        "https://i.example.com/authorize",
                        "https://i.example.com/token",
                        List.of("plain"));
        AuruException error =
                assertThrows(
                        AuruException.class,
                        () -> OAuth.beginAuthorization(plainOnly, browserClient(), "openid"));
        assertTrue(error.getMessage().contains("PKCE S256"), error.getMessage());
    }

    @Test
    void refusesAClientNotRegisteredForTheFlow() {
        OAuthConfiguration.OAuthClient deviceOnly =
                new OAuthConfiguration.OAuthClient(
                        OAuthConfiguration.Kind.BROWSER,
                        "dashboard",
                        "https://dashboard.example.com/cb",
                        List.of("device_authorization"));
        AuruException error =
                assertThrows(
                        AuruException.class,
                        () -> OAuth.beginAuthorization(metadata(), deviceOnly, "openid"));
        assertTrue(error.getMessage().contains("authorization_code_pkce"), error.getMessage());
    }

    @Test
    void carriesExtraAuthorizationParameters() {
        OAuth.AuthorizationRequest request =
                OAuth.beginAuthorization(
                        metadata(), browserClient(), "openid", Map.of("prompt", "consent"));
        assertEquals("consent", query(request.url()).get("prompt"));
    }

    // ── The redirect ─────────────────────────────────────────────────────────

    @Test
    void refusesARedirectWhoseStateDoesNotMatch() {
        OAuth.AuthorizationRequest request =
                OAuth.beginAuthorization(metadata(), browserClient(), "openid");
        AuruException error =
                assertThrows(
                        AuruException.class,
                        () -> request.codeFrom("https://dashboard.example.com/cb?code=abc&state=someone-else"));
        assertTrue(error.getMessage().contains("state does not match"), error.getMessage());
    }

    @Test
    void surfacesAnErrorReturnedInsteadOfACode() {
        OAuth.AuthorizationRequest request =
                OAuth.beginAuthorization(metadata(), browserClient(), "openid");
        AuruException error =
                assertThrows(
                        AuruException.class,
                        () ->
                                request.codeFrom(
                                        "https://dashboard.example.com/cb?error=access_denied"
                                                + "&error_description=user+said+no&state="
                                                + request.state()));
        assertTrue(error.getMessage().contains("user said no"), error.getMessage());
    }

    @Test
    void pullsTheCodeOutOfAMatchingRedirect() {
        OAuth.AuthorizationRequest request =
                OAuth.beginAuthorization(metadata(), browserClient(), "openid");
        assertEquals(
                "the-code",
                request.codeFrom(
                        "https://dashboard.example.com/cb?code=the-code&state=" + request.state()));
    }

    // ── Completing authorization ─────────────────────────────────────────────

    private OAuth.AuthorizationRequest preparedAgainstStub() throws IOException {
        return OAuth.beginAuthorization(OAuth.discover(issuer, new JdkHttpTransport()), browserClient(), "openid");
    }

    @Test
    void discardsTheRefreshTokenByDefault() throws IOException {
        startIdentityProvider(discoveryDocument(), 200, TOKEN_RESPONSE);
        OAuth.ServerMetadata metadata = OAuth.discover(issuer, new JdkHttpTransport());
        OAuth.AuthorizationRequest request =
                OAuth.beginAuthorization(metadata, browserClient(), "openid");

        OAuth.AccessToken token =
                OAuth.completeAuthorization(metadata, browserClient(), request, "the-code", new JdkHttpTransport());

        assertEquals("an-access-token", token.token());
        assertEquals(Optional.of(Duration.ofSeconds(3600)), token.expiresIn());
        // There is no accessor that could return it, which is the point.
        assertTrue(!token.toString().contains("a-refresh-token"));
    }

    @Test
    void returnsTheRefreshTokenOnlyWhenAskedForExplicitly() throws IOException {
        startIdentityProvider(discoveryDocument(), 200, TOKEN_RESPONSE);
        OAuth.ServerMetadata metadata = OAuth.discover(issuer, new JdkHttpTransport());
        OAuth.RefreshableToken token =
                OAuth.completeAuthorizationWithRefresh(
                        metadata,
                        browserClient(),
                        OAuth.beginAuthorization(metadata, browserClient(), "openid"),
                        "the-code", new JdkHttpTransport());
        assertEquals(Optional.of("a-refresh-token"), token.refreshToken());
    }

    @Test
    void sendsTheVerifierAndNoClientSecret() throws IOException {
        startIdentityProvider(discoveryDocument(), 200, TOKEN_RESPONSE);
        OAuth.ServerMetadata metadata = OAuth.discover(issuer, new JdkHttpTransport());
        OAuth.AuthorizationRequest request =
                OAuth.beginAuthorization(metadata, browserClient(), "openid");
        OAuth.completeAuthorization(metadata, browserClient(), request, "the-code", new JdkHttpTransport());

        String sent = lastTokenBody.get();
        assertTrue(sent.contains("grant_type=authorization_code"), sent);
        assertTrue(sent.contains("code_verifier=" + request.codeVerifier()), sent);
        assertTrue(sent.contains("client_id=dashboard"), sent);
        // A public client has no secret; sending one would mean it was shipped.
        assertTrue(!sent.contains("client_secret"), sent);
    }

    @Test
    void reportsTheProvidersErrorDescriptionWhenExchangeFails() throws IOException {
        startIdentityProvider(
                discoveryDocument(),
                400,
                "{\"error\":\"invalid_grant\",\"error_description\":\"code already used\"}");
        OAuth.ServerMetadata metadata = OAuth.discover(issuer, new JdkHttpTransport());
        AuruException error =
                assertThrows(
                        AuruException.class,
                        () ->
                                OAuth.completeAuthorization(
                                        metadata,
                                        browserClient(),
                                        OAuth.beginAuthorization(metadata, browserClient(), "openid"),
                                        "expired", new JdkHttpTransport()));
        assertTrue(error.getMessage().contains("code already used"), error.getMessage());
    }

    @Test
    void refusesASuccessResponseCarryingNoAccessToken() throws IOException {
        startIdentityProvider(discoveryDocument(), 200, "{\"token_type\":\"Bearer\"}");
        OAuth.ServerMetadata metadata = OAuth.discover(issuer, new JdkHttpTransport());
        AuruException error =
                assertThrows(
                        AuruException.class,
                        () ->
                                OAuth.completeAuthorization(
                                        metadata,
                                        browserClient(),
                                        OAuth.beginAuthorization(metadata, browserClient(), "openid"),
                                        "the-code", new JdkHttpTransport()));
        assertTrue(error.getMessage().contains("no access_token"), error.getMessage());
    }
}

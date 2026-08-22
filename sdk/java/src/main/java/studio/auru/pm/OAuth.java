package studio.auru.pm;

import java.io.IOException;
import java.net.URI;
import java.net.URLEncoder;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.security.SecureRandom;
import java.time.Duration;
import java.util.Base64;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Optional;

/**
 * OAuth 2.0 Authorization Code with PKCE, for providers that advertise it.
 *
 * <p>Two things here are deliberate and worth reading before changing them.
 *
 * <p><strong>Endpoints come from discovery, never from the provider's health document.</strong> A
 * provider publishes only its issuer; {@link #discover} fetches that issuer's RFC 8414 or OpenID
 * Connect metadata and rejects a document whose {@code issuer} is not byte-identical to the one
 * asked for. A provider that could name its own token endpoint could name someone else's.
 *
 * <p><strong>The refresh token is discarded by default.</strong> {@link #completeAuthorization}
 * returns only a short-lived access token, held in memory. A caller who is handed a refresh token
 * will eventually store it, and most places it could be stored are worse than not having it at
 * all. {@link #completeAuthorizationWithRefresh} is the explicit opt-in for callers that have a
 * real secret store — an OS keychain, a secrets manager — and never for code that logs its
 * configuration.
 */
public final class OAuth {

    private static final SecureRandom RANDOM = new SecureRandom();
    private static final Base64.Encoder BASE64_URL = Base64.getUrlEncoder().withoutPadding();

    private OAuth() {}

    /** The subset of authorization-server metadata this flow needs. */
    public record ServerMetadata(
            String issuer,
            String authorizationEndpoint,
            String tokenEndpoint,
            List<String> codeChallengeMethodsSupported) {

        public ServerMetadata {
            codeChallengeMethodsSupported = List.copyOf(codeChallengeMethodsSupported);
        }
    }

    /** An access token, held for as long as the caller keeps it and written nowhere. */
    public record AccessToken(
            String token, String tokenType, Optional<Duration> expiresIn, Optional<String> scope) {}

    /**
     * An access token together with its refresh token.
     *
     * <p>Only produced by {@link #completeAuthorizationWithRefresh}.
     */
    public record RefreshableToken(AccessToken access, Optional<String> refreshToken) {}

    /** A prepared authorization request. */
    public record AuthorizationRequest(String url, String state, String codeVerifier) {

        /**
         * Check the redirect's {@code state} and pull out the code.
         *
         * @throws AuruException if the state does not match this request, or the provider returned
         *     an error instead of a code
         */
        public String codeFrom(String redirectUrl) {
            Map<String, String> parameters = parseQuery(URI.create(redirectUrl).getRawQuery());

            String returnedState = parameters.get("state");
            if (!state.equals(returnedState)) {
                throw new AuruException(
                        ErrorCode.UNAUTHORIZED,
                        "authorization state does not match the request; the response is not ours");
            }
            String error = parameters.get("error");
            if (error != null) {
                String description = parameters.getOrDefault("error_description", error);
                throw new AuruException(ErrorCode.UNAUTHORIZED, "authorization failed: " + description);
            }
            String code = parameters.get("code");
            if (code == null) {
                throw new AuruException(ErrorCode.UNAUTHORIZED, "the redirect carried no code");
            }
            return code;
        }
    }

    /**
     * Fetch and validate an issuer's authorization-server metadata.
     *
     * <p>Tries OpenID Connect discovery, then RFC 8414.
     *
     * @throws AuruException if neither is reachable, if the document names a different issuer, or
     *     if it omits the endpoints the flow needs
     */
    public static ServerMetadata discover(String issuer, Transport transport) {
        String base = issuer.replaceAll("/+$", "");
        List<String> candidates =
                List.of(
                        base + "/.well-known/openid-configuration",
                        base + "/.well-known/oauth-authorization-server");

        String lastProblem = "no discovery document";
        for (String url : candidates) {
            Transport.Response response;
            try {
                response =
                        transport.send(
                                new Transport.Request(
                                        "GET", url, List.of(Map.entry("accept", "application/json")), null));
            } catch (IOException cause) {
                lastProblem = url + ": " + cause;
                continue;
            }

            if (response.status() != 200) {
                lastProblem = url + ": HTTP " + response.status();
                continue;
            }

            Json document = Json.parse(new String(response.body(), StandardCharsets.UTF_8));
            String declared = document.optString("issuer").orElse("");
            if (!issuer.equals(declared)) {
                throw new AuruException(
                        ErrorCode.UNAUTHORIZED,
                        "discovery at " + url + " claims issuer " + declared + ", expected " + issuer);
            }

            Optional<String> authorization = document.optString("authorization_endpoint");
            Optional<String> token = document.optString("token_endpoint");
            if (authorization.isEmpty() || token.isEmpty()) {
                throw new AuruException(
                        ErrorCode.UNSUPPORTED, url + " omits the endpoints PKCE needs");
            }

            return new ServerMetadata(
                    declared,
                    authorization.get(),
                    token.get(),
                    document.get("code_challenge_methods_supported").orElse(Json.NULL).elements()
                            .stream()
                            .map(value -> ((Json.Str) value).value())
                            .toList());
        }

        throw new AuruException(
                ErrorCode.UNAUTHORIZED, "cannot discover " + issuer + ": " + lastProblem);
    }

    /**
     * Build an authorization URL and the PKCE secret that completes it.
     *
     * <p>Refuses a provider that does not advertise {@code S256}. A plain challenge is not a
     * fallback worth having: the point of PKCE is that an intercepted authorization code is
     * useless, which a plain challenge does not give you.
     *
     * @param scope usually {@link OAuthConfiguration#requiredScope()}
     */
    public static AuthorizationRequest beginAuthorization(
            ServerMetadata metadata, OAuthConfiguration.OAuthClient client, String scope) {
        return beginAuthorization(metadata, client, scope, Map.of());
    }

    /** As above, with extra authorization parameters such as {@code prompt} or {@code login_hint}. */
    public static AuthorizationRequest beginAuthorization(
            ServerMetadata metadata,
            OAuthConfiguration.OAuthClient client,
            String scope,
            Map<String, String> extraParameters) {

        List<String> methods = metadata.codeChallengeMethodsSupported();
        if (!methods.isEmpty() && !methods.contains("S256")) {
            throw new AuruException(
                    ErrorCode.UNSUPPORTED, metadata.issuer() + " does not support PKCE S256");
        }
        if (!client.flows().contains("authorization_code_pkce")) {
            throw new AuruException(
                    ErrorCode.UNSUPPORTED,
                    "client " + client.clientId() + " is not registered for authorization_code_pkce");
        }

        String codeVerifier = randomUrlSafe(32);
        String state = randomUrlSafe(16);

        Map<String, String> parameters = new LinkedHashMap<>();
        parameters.put("response_type", "code");
        parameters.put("client_id", client.clientId());
        parameters.put("redirect_uri", client.redirectUri());
        parameters.put("scope", scope);
        parameters.put("state", state);
        parameters.put("code_challenge", s256(codeVerifier));
        parameters.put("code_challenge_method", "S256");
        parameters.putAll(extraParameters);

        String separator = metadata.authorizationEndpoint().contains("?") ? "&" : "?";
        return new AuthorizationRequest(
                metadata.authorizationEndpoint() + separator + form(parameters), state, codeVerifier);
    }

    /**
     * Exchange an authorization code for an access token.
     *
     * <p>The refresh token, if the provider issued one, is discarded rather than returned. When the
     * access token expires, run the flow again.
     */
    public static AccessToken completeAuthorization(
            ServerMetadata metadata,
            OAuthConfiguration.OAuthClient client,
            AuthorizationRequest request,
            String code,
            Transport transport) {
        return completeAuthorizationWithRefresh(metadata, client, request, code, transport).access();
    }

    /**
     * Exchange an authorization code, keeping the refresh token.
     *
     * <p>Only for callers with a real secret store. A refresh token is a long-lived credential for
     * the whole account; treat it the way you would treat a password.
     */
    public static RefreshableToken completeAuthorizationWithRefresh(
            ServerMetadata metadata,
            OAuthConfiguration.OAuthClient client,
            AuthorizationRequest request,
            String code,
            Transport transport) {

        Map<String, String> body = new LinkedHashMap<>();
        body.put("grant_type", "authorization_code");
        body.put("code", code);
        body.put("redirect_uri", client.redirectUri());
        body.put("client_id", client.clientId());
        body.put("code_verifier", request.codeVerifier());
        // A public client has no secret. Sending one would mean it had been
        // shipped to wherever this code runs.

        Transport.Response response;
        try {
            response =
                    transport.send(
                            new Transport.Request(
                                    "POST",
                                    metadata.tokenEndpoint(),
                                    List.of(
                                            Map.entry(
                                                    "content-type",
                                                    "application/x-www-form-urlencoded"),
                                            Map.entry("accept", "application/json")),
                                    form(body).getBytes(StandardCharsets.UTF_8)));
        } catch (IOException cause) {
            throw new AuruException(
                    ErrorCode.INTERNAL, "cannot reach " + metadata.tokenEndpoint() + ": " + cause, cause);
        }

        Json parsed;
        try {
            parsed = Json.parse(new String(response.body(), StandardCharsets.UTF_8));
        } catch (RuntimeException notJson) {
            parsed = Json.object().build();
        }
        final Json payload = parsed;

        if (response.status() / 100 != 2) {
            String detail =
                    payload.optString("error_description")
                            .or(() -> payload.optString("error"))
                            .orElseGet(() -> "HTTP " + response.status());
            throw new AuruException(ErrorCode.UNAUTHORIZED, "token exchange failed: " + detail);
        }

        Optional<String> token = payload.optString("access_token");
        if (token.isEmpty()) {
            throw new AuruException(ErrorCode.UNAUTHORIZED, "the token response carried no access_token");
        }

        Optional<Duration> expiresIn = Optional.empty();
        if (payload.get("expires_in").orElse(Json.NULL) instanceof Json.Int seconds) {
            expiresIn = Optional.of(Duration.ofSeconds(seconds.value()));
        }

        return new RefreshableToken(
                new AccessToken(
                        token.get(),
                        payload.optString("token_type").orElse("bearer"),
                        expiresIn,
                        payload.optString("scope")),
                payload.optString("refresh_token"));
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    private static String randomUrlSafe(int byteLength) {
        byte[] bytes = new byte[byteLength];
        RANDOM.nextBytes(bytes);
        return BASE64_URL.encodeToString(bytes);
    }

    private static String s256(String verifier) {
        try {
            MessageDigest digest = MessageDigest.getInstance("SHA-256");
            return BASE64_URL.encodeToString(digest.digest(verifier.getBytes(StandardCharsets.US_ASCII)));
        } catch (NoSuchAlgorithmException impossible) {
            // Every conformant JRE ships SHA-256.
            throw new IllegalStateException("SHA-256 is unavailable", impossible);
        }
    }

    private static String form(Map<String, String> parameters) {
        StringBuilder encoded = new StringBuilder();
        for (Map.Entry<String, String> parameter : parameters.entrySet()) {
            if (encoded.length() > 0) {
                encoded.append('&');
            }
            encoded
                    .append(URLEncoder.encode(parameter.getKey(), StandardCharsets.UTF_8))
                    .append('=')
                    .append(URLEncoder.encode(parameter.getValue(), StandardCharsets.UTF_8));
        }
        return encoded.toString();
    }

    private static Map<String, String> parseQuery(String rawQuery) {
        Map<String, String> parameters = new LinkedHashMap<>();
        if (rawQuery == null || rawQuery.isEmpty()) {
            return parameters;
        }
        for (String pair : rawQuery.split("&")) {
            int equals = pair.indexOf('=');
            if (equals < 0) {
                continue;
            }
            parameters.put(
                    java.net.URLDecoder.decode(pair.substring(0, equals), StandardCharsets.UTF_8),
                    java.net.URLDecoder.decode(pair.substring(equals + 1), StandardCharsets.UTF_8));
        }
        return parameters;
    }
}

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
 * OAuth 2.0 for providers that advertise it: Authorization Code with PKCE, and the RFC 8628 device
 * authorization grant.
 *
 * <p>The two differ in where the person authenticates. PKCE sends them to a browser this
 * application launched and reads the answer off a redirect it is listening for, which needs a
 * redirect URI the identity provider has been told to allow. The device grant sends them anywhere
 * at all — another device, if they like — and learns the answer by polling, which needs no redirect
 * and so no per-platform client registration. On a phone, where a loopback redirect is not
 * available, that difference is the whole reason the device grant is the path that works.
 *
 * <p>Two more things are deliberate and worth reading before changing them.
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
            List<String> codeChallengeMethodsSupported,
            Optional<String> deviceAuthorizationEndpoint) {

        public ServerMetadata {
            codeChallengeMethodsSupported = List.copyOf(codeChallengeMethodsSupported);
        }

        /** As above, for a provider that publishes no device authorization endpoint. */
        public ServerMetadata(
                String issuer,
                String authorizationEndpoint,
                String tokenEndpoint,
                List<String> codeChallengeMethodsSupported) {
            this(
                    issuer,
                    authorizationEndpoint,
                    tokenEndpoint,
                    codeChallengeMethodsSupported,
                    Optional.empty());
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
                            .toList(),
                    document.optString("device_authorization_endpoint"));
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

        Transport.Response response = postForm(metadata.tokenEndpoint(), body, transport);
        Json payload = parseJsonBody(response);

        if (response.status() / 100 != 2) {
            throw new AuruException(
                    ErrorCode.UNAUTHORIZED,
                    "token exchange failed: " + failureDetail(payload, response.status()));
        }
        return readToken(payload);
    }

    // ── Device authorization (RFC 8628) ──────────────────────────────────────

    /**
     * A device authorization, as issued by the authorization server.
     *
     * <p>Show {@link #userCode()} and {@link #verificationUri()} to the person signing in. {@link
     * #deviceCode()} is a secret used only to poll with, and must never be displayed.
     */
    public record DeviceAuthorization(
            String deviceCode,
            String userCode,
            String verificationUri,
            Optional<String> verificationUriComplete,
            Duration expiresIn,
            Duration interval) {}

    /**
     * The outcome of one poll.
     *
     * <p>Only the two outcomes that are not failures appear here. Everything terminal — the person
     * refused, the code expired, the server rejected the client — arrives as an {@link
     * AuruException}, because a caller that has to pattern-match on failure to notice it will
     * eventually forget to.
     */
    public sealed interface DevicePollResult {

        /** Nobody has approved it yet. Wait {@code retryAfter}, then poll again. */
        record Pending(Duration retryAfter) implements DevicePollResult {}

        /** Approved, and the token is here. */
        record Authorized(RefreshableToken token) implements DevicePollResult {}
    }

    /**
     * Ask the authorization server for a device code.
     *
     * <p>Unlike the authorization-code flow this needs no redirect URI, no browser the app can
     * observe, and no PKCE verifier: the person authenticates somewhere else entirely and this
     * client learns about it by polling. That is why it works on a phone with no client
     * registration changes at all.
     *
     * @param scope usually {@link OAuthConfiguration#requiredScope()}
     * @throws AuruException if the provider publishes no device authorization endpoint, if the
     *     client is not registered for the flow, or if the server refuses the request
     */
    public static DeviceAuthorization beginDeviceAuthorization(
            ServerMetadata metadata,
            OAuthConfiguration.OAuthClient client,
            String scope,
            Transport transport) {

        String endpoint =
                metadata.deviceAuthorizationEndpoint()
                        .orElseThrow(
                                () ->
                                        new AuruException(
                                                ErrorCode.UNSUPPORTED,
                                                metadata.issuer()
                                                        + " publishes no device authorization endpoint"));
        if (!client.flows().contains("device_authorization")) {
            throw new AuruException(
                    ErrorCode.UNSUPPORTED,
                    "client " + client.clientId() + " is not registered for device_authorization");
        }

        Map<String, String> body = new LinkedHashMap<>();
        body.put("client_id", client.clientId());
        body.put("scope", scope);

        Transport.Response response = postForm(endpoint, body, transport);
        Json payload = parseJsonBody(response);

        if (response.status() / 100 != 2) {
            throw new AuruException(
                    ErrorCode.UNAUTHORIZED,
                    "device authorization failed: " + failureDetail(payload, response.status()));
        }

        return new DeviceAuthorization(
                required(payload, "device_code"),
                required(payload, "user_code"),
                required(payload, "verification_uri"),
                payload.optString("verification_uri_complete"),
                // RFC 8628 makes both optional. Its own defaults are the only
                // honest guess, and guessing beats polling forever.
                seconds(payload, "expires_in", 300),
                seconds(payload, "interval", 5));
    }

    /**
     * Poll once for the token.
     *
     * @param interval how long the caller waited before this attempt — {@link
     *     DeviceAuthorization#interval()} for the first poll, and thereafter whatever the previous
     *     {@link DevicePollResult.Pending#retryAfter()} said. RFC 8628 widens the interval
     *     permanently once a server has answered {@code slow_down}, so a caller that keeps passing
     *     the original value will keep being told to slow down.
     * @throws AuruException if the person refused, the code expired, or the request was rejected
     */
    public static DevicePollResult pollDeviceAuthorization(
            ServerMetadata metadata,
            OAuthConfiguration.OAuthClient client,
            DeviceAuthorization authorization,
            Duration interval,
            Transport transport) {

        Map<String, String> body = new LinkedHashMap<>();
        body.put("grant_type", "urn:ietf:params:oauth:grant-type:device_code");
        body.put("device_code", authorization.deviceCode());
        body.put("client_id", client.clientId());

        Transport.Response response = postForm(metadata.tokenEndpoint(), body, transport);
        Json payload = parseJsonBody(response);

        // The body decides, not the status. RFC 8628 carries the pending signal
        // in an OAuth error object, which OAuth 2.0 returns as 400 — but this
        // protocol's own legacy endpoint returns the same object as 200. Reading
        // the body first works for both; the status only matters when there is no
        // error code in it to read.
        Optional<String> error = payload.optString("error");
        if (error.isPresent()) {
            String code = error.get();
            if ("authorization_pending".equals(code)) {
                return new DevicePollResult.Pending(interval);
            }
            if ("slow_down".equals(code)) {
                return new DevicePollResult.Pending(interval.plusSeconds(5));
            }
            if ("access_denied".equals(code)) {
                throw new AuruException(ErrorCode.UNAUTHORIZED, "the sign-in request was refused");
            }
            if ("expired_token".equals(code)) {
                throw new AuruException(
                        ErrorCode.UNAUTHORIZED, "the sign-in code expired. Start again for a new one.");
            }
            throw new AuruException(
                    ErrorCode.UNAUTHORIZED,
                    "device authorization failed: "
                            + payload.optString("error_description").orElse(code));
        }

        if (response.status() / 100 != 2) {
            throw new AuruException(
                    ErrorCode.UNAUTHORIZED,
                    "device authorization failed: " + failureDetail(payload, response.status()));
        }
        return new DevicePollResult.Authorized(readToken(payload));
    }

    /**
     * Poll until the person approves, discarding the refresh token.
     *
     * <p>Blocks for as long as the code is valid — minutes — so call it off whatever thread must
     * stay responsive. Interrupting the thread abandons the wait.
     */
    public static AccessToken completeDeviceAuthorization(
            ServerMetadata metadata,
            OAuthConfiguration.OAuthClient client,
            DeviceAuthorization authorization,
            Transport transport) {
        return completeDeviceAuthorizationWithRefresh(metadata, client, authorization, transport)
                .access();
    }

    /**
     * Poll until the person approves, keeping the refresh token.
     *
     * <p>Only for callers with a real secret store, for the reason given on {@link
     * #completeAuthorizationWithRefresh}.
     */
    public static RefreshableToken completeDeviceAuthorizationWithRefresh(
            ServerMetadata metadata,
            OAuthConfiguration.OAuthClient client,
            DeviceAuthorization authorization,
            Transport transport) {

        long deadline = System.nanoTime() + authorization.expiresIn().toNanos();
        Duration interval = authorization.interval();

        while (true) {
            if (System.nanoTime() >= deadline) {
                throw new AuruException(
                        ErrorCode.UNAUTHORIZED, "the sign-in code expired. Start again for a new one.");
            }
            try {
                Thread.sleep(interval.toMillis());
            } catch (InterruptedException interrupted) {
                Thread.currentThread().interrupt();
                throw new AuruException(
                        ErrorCode.INTERNAL, "interrupted while waiting for sign-in", interrupted);
            }

            DevicePollResult result =
                    pollDeviceAuthorization(metadata, client, authorization, interval, transport);
            if (result instanceof DevicePollResult.Authorized authorized) {
                return authorized.token();
            }
            interval = ((DevicePollResult.Pending) result).retryAfter();
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    private static Transport.Response postForm(
            String url, Map<String, String> body, Transport transport) {
        try {
            return transport.send(
                    new Transport.Request(
                            "POST",
                            url,
                            List.of(
                                    Map.entry("content-type", "application/x-www-form-urlencoded"),
                                    Map.entry("accept", "application/json")),
                            form(body).getBytes(StandardCharsets.UTF_8)));
        } catch (IOException cause) {
            throw new AuruException(ErrorCode.INTERNAL, "cannot reach " + url + ": " + cause, cause);
        }
    }

    /** The body as JSON, or an empty object when it is not JSON at all. */
    private static Json parseJsonBody(Transport.Response response) {
        try {
            return Json.parse(new String(response.body(), StandardCharsets.UTF_8));
        } catch (RuntimeException notJson) {
            return Json.object().build();
        }
    }

    private static String failureDetail(Json payload, int status) {
        return payload.optString("error_description")
                .or(() -> payload.optString("error"))
                .orElseGet(() -> "HTTP " + status);
    }

    private static RefreshableToken readToken(Json payload) {
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

    private static String required(Json payload, String field) {
        return payload.optString(field)
                .orElseThrow(
                        () ->
                                new AuruException(
                                        ErrorCode.UNAUTHORIZED,
                                        "the device authorization response carried no " + field));
    }

    private static Duration seconds(Json payload, String field, long fallback) {
        if (payload.get(field).orElse(Json.NULL) instanceof Json.Int value) {
            return Duration.ofSeconds(value.value());
        }
        return Duration.ofSeconds(fallback);
    }

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

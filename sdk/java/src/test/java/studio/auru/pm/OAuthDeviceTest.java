package studio.auru.pm;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.net.URLDecoder;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Deque;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import org.junit.jupiter.api.Test;

/**
 * The RFC 8628 device authorization grant.
 *
 * <p>These run against a scripted authorization server rather than a real one, which is the
 * exception to this suite's usual rule. {@link TestProvider} exists because mocked HTTP only proves
 * a client agrees with the test's idea of the protocol — but {@code auru-pm-server} is a resource
 * server and issues no device codes at all, so there is no reference implementation here to
 * interoperate with. What is being pinned down is this client's half: which parameters it sends,
 * and how it reads each of the four RFC 8628 error codes back.
 *
 * <p>The polling rules are where a device flow actually goes wrong. A client that treats {@code
 * slow_down} as a failure locks the person out; one that ignores it hammers the server into
 * rate-limiting them; one that reads the pending signal off the HTTP status rather than the body
 * works against one provider and not the next.
 */
class OAuthDeviceTest {

    private static final String ISSUER = "https://identity.example.com";

    /** An authorization server that answers with whatever the test queued. */
    private static final class ScriptedServer implements Transport {

        private final Deque<Transport.Response> responses = new ArrayDeque<>();
        private final List<Transport.Request> received = new ArrayList<>();

        ScriptedServer answering(int status, String body) {
            responses.add(
                    new Transport.Response(status, List.of(), body.getBytes(StandardCharsets.UTF_8)));
            return this;
        }

        @Override
        public Transport.Response send(Transport.Request request) {
            received.add(request);
            if (responses.isEmpty()) {
                throw new AssertionError("the client made more requests than the test scripted");
            }
            return responses.removeFirst();
        }

        Transport.Request lastRequest() {
            return received.get(received.size() - 1);
        }

        Map<String, String> lastForm() {
            Map<String, String> fields = new HashMap<>();
            String body = new String(lastRequest().body(), StandardCharsets.UTF_8);
            for (String pair : body.split("&")) {
                int equals = pair.indexOf('=');
                fields.put(
                        URLDecoder.decode(pair.substring(0, equals), StandardCharsets.UTF_8),
                        URLDecoder.decode(pair.substring(equals + 1), StandardCharsets.UTF_8));
            }
            return fields;
        }
    }

    private static OAuth.ServerMetadata metadata() {
        return new OAuth.ServerMetadata(
                ISSUER,
                ISSUER + "/authorize",
                ISSUER + "/token",
                List.of("S256"),
                Optional.of(ISSUER + "/device"));
    }

    private static OAuthConfiguration.OAuthClient client() {
        return new OAuthConfiguration.OAuthClient(
                OAuthConfiguration.Kind.NATIVE,
                "auru-pm-desktop",
                "http://127.0.0.1:43827/oauth/callback",
                List.of("authorization_code_pkce", "device_authorization"));
    }

    private static final String DEVICE_RESPONSE =
            "{\"device_code\":\"a-device-code\",\"user_code\":\"ABCD-1234\","
                    + "\"verification_uri\":\"https://identity.example.com/activate\","
                    + "\"verification_uri_complete\":\"https://identity.example.com/activate?user_code=ABCD-1234\","
                    + "\"expires_in\":600,\"interval\":3}";

    private static final String TOKEN_RESPONSE =
            "{\"access_token\":\"an-access-token\",\"refresh_token\":\"a-refresh-token\","
                    + "\"token_type\":\"Bearer\",\"expires_in\":3600,\"scope\":\"openid\"}";

    private static OAuth.DeviceAuthorization authorization() {
        return new OAuth.DeviceAuthorization(
                "a-device-code",
                "ABCD-1234",
                "https://identity.example.com/activate",
                Optional.empty(),
                Duration.ofSeconds(600),
                Duration.ofSeconds(3));
    }

    // ── Discovery ────────────────────────────────────────────────────────────

    @Test
    void discoveryCarriesTheDeviceEndpointWhenTheProviderPublishesOne() {
        ScriptedServer server =
                new ScriptedServer()
                        .answering(
                                200,
                                "{\"issuer\":\"" + ISSUER + "\","
                                        + "\"authorization_endpoint\":\"" + ISSUER + "/authorize\","
                                        + "\"token_endpoint\":\"" + ISSUER + "/token\","
                                        + "\"device_authorization_endpoint\":\"" + ISSUER + "/device\","
                                        + "\"code_challenge_methods_supported\":[\"S256\"]}");

        OAuth.ServerMetadata discovered = OAuth.discover(ISSUER, server);

        assertEquals(Optional.of(ISSUER + "/device"), discovered.deviceAuthorizationEndpoint());
    }

    @Test
    void discoveryLeavesTheDeviceEndpointEmptyWhenThereIsNone() {
        ScriptedServer server =
                new ScriptedServer()
                        .answering(
                                200,
                                "{\"issuer\":\"" + ISSUER + "\","
                                        + "\"authorization_endpoint\":\"" + ISSUER + "/authorize\","
                                        + "\"token_endpoint\":\"" + ISSUER + "/token\"}");

        assertTrue(OAuth.discover(ISSUER, server).deviceAuthorizationEndpoint().isEmpty());
    }

    // ── Asking for a code ────────────────────────────────────────────────────

    @Test
    void asksTheDeviceEndpointForACodeAndReadsItBack() {
        ScriptedServer server = new ScriptedServer().answering(200, DEVICE_RESPONSE);

        OAuth.DeviceAuthorization granted =
                OAuth.beginDeviceAuthorization(metadata(), client(), "openid", server);

        assertEquals(ISSUER + "/device", server.lastRequest().url());
        assertEquals("POST", server.lastRequest().method());
        assertEquals("auru-pm-desktop", server.lastForm().get("client_id"));
        assertEquals("openid", server.lastForm().get("scope"));

        assertEquals("a-device-code", granted.deviceCode());
        assertEquals("ABCD-1234", granted.userCode());
        assertEquals("https://identity.example.com/activate", granted.verificationUri());
        assertEquals(
                Optional.of("https://identity.example.com/activate?user_code=ABCD-1234"),
                granted.verificationUriComplete());
        assertEquals(Duration.ofSeconds(600), granted.expiresIn());
        assertEquals(Duration.ofSeconds(3), granted.interval());
    }

    @Test
    void sendsNoRedirectUriAndNoPkceVerifier() {
        // The whole reason this flow works on a phone: nothing here depends on a
        // redirect the identity provider had to be told to allow.
        ScriptedServer server = new ScriptedServer().answering(200, DEVICE_RESPONSE);

        OAuth.beginDeviceAuthorization(metadata(), client(), "openid", server);

        assertFalse(server.lastForm().containsKey("redirect_uri"));
        assertFalse(server.lastForm().containsKey("code_challenge"));
    }

    @Test
    void fallsBackToTheSpecsOwnDefaultsWhenTheServerOmitsTheTimings() {
        ScriptedServer server =
                new ScriptedServer()
                        .answering(
                                200,
                                "{\"device_code\":\"a-device-code\",\"user_code\":\"ABCD-1234\","
                                        + "\"verification_uri\":\"https://identity.example.com/activate\"}");

        OAuth.DeviceAuthorization granted =
                OAuth.beginDeviceAuthorization(metadata(), client(), "openid", server);

        assertEquals(Duration.ofSeconds(300), granted.expiresIn());
        assertEquals(Duration.ofSeconds(5), granted.interval());
    }

    @Test
    void refusesAProviderThatPublishesNoDeviceEndpoint() {
        OAuth.ServerMetadata withoutDevice =
                new OAuth.ServerMetadata(
                        ISSUER, ISSUER + "/authorize", ISSUER + "/token", List.of("S256"));

        AuruException error =
                assertThrows(
                        AuruException.class,
                        () ->
                                OAuth.beginDeviceAuthorization(
                                        withoutDevice, client(), "openid", new ScriptedServer()));

        assertEquals(ErrorCode.UNSUPPORTED, error.code());
        assertTrue(error.getMessage().contains("publishes no device authorization endpoint"), error.getMessage());
    }

    @Test
    void refusesAClientNotRegisteredForTheFlow() {
        // Registration is per flow. A client id allowed to run PKCE is not
        // thereby allowed to run this, and finding out from the server costs a
        // round trip and a worse error message.
        OAuthConfiguration.OAuthClient pkceOnly =
                new OAuthConfiguration.OAuthClient(
                        OAuthConfiguration.Kind.NATIVE,
                        "auru-pm-desktop",
                        "http://127.0.0.1:43827/oauth/callback",
                        List.of("authorization_code_pkce"));

        AuruException error =
                assertThrows(
                        AuruException.class,
                        () ->
                                OAuth.beginDeviceAuthorization(
                                        metadata(), pkceOnly, "openid", new ScriptedServer()));

        assertEquals(ErrorCode.UNSUPPORTED, error.code());
        assertTrue(error.getMessage().contains("device_authorization"), error.getMessage());
    }

    @Test
    void refusesADeviceResponseMissingTheCodeItMustPollWith() {
        ScriptedServer server =
                new ScriptedServer()
                        .answering(
                                200,
                                "{\"user_code\":\"ABCD-1234\","
                                        + "\"verification_uri\":\"https://identity.example.com/activate\"}");

        AuruException error =
                assertThrows(
                        AuruException.class,
                        () -> OAuth.beginDeviceAuthorization(metadata(), client(), "openid", server));

        assertTrue(error.getMessage().contains("device_code"), error.getMessage());
    }

    // ── Polling ──────────────────────────────────────────────────────────────

    private static OAuth.DevicePollResult poll(ScriptedServer server, Duration interval) {
        return OAuth.pollDeviceAuthorization(metadata(), client(), authorization(), interval, server);
    }

    @Test
    void pollsTheTokenEndpointWithTheDeviceCodeGrant() {
        ScriptedServer server = new ScriptedServer().answering(200, TOKEN_RESPONSE);

        poll(server, Duration.ofSeconds(3));

        assertEquals(ISSUER + "/token", server.lastRequest().url());
        assertEquals(
                "urn:ietf:params:oauth:grant-type:device_code", server.lastForm().get("grant_type"));
        assertEquals("a-device-code", server.lastForm().get("device_code"));
        assertEquals("auru-pm-desktop", server.lastForm().get("client_id"));
    }

    @Test
    void readsTheTokenOnceTheCodeIsApproved() {
        ScriptedServer server = new ScriptedServer().answering(200, TOKEN_RESPONSE);

        OAuth.DevicePollResult result = poll(server, Duration.ofSeconds(3));

        OAuth.DevicePollResult.Authorized authorized =
                assertInstanceOf(OAuth.DevicePollResult.Authorized.class, result);
        assertEquals("an-access-token", authorized.token().access().token());
        assertEquals(Optional.of("a-refresh-token"), authorized.token().refreshToken());
        assertEquals(Optional.of(Duration.ofSeconds(3600)), authorized.token().access().expiresIn());
    }

    @Test
    void keepsWaitingWhileNobodyHasApprovedItYet() {
        ScriptedServer server = new ScriptedServer().answering(400, "{\"error\":\"authorization_pending\"}");

        OAuth.DevicePollResult result = poll(server, Duration.ofSeconds(3));

        assertEquals(
                Duration.ofSeconds(3),
                assertInstanceOf(OAuth.DevicePollResult.Pending.class, result).retryAfter());
    }

    @Test
    void readsThePendingSignalFromTheBodyWhateverStatusCarriesIt() {
        // OAuth 2.0 returns an error object as 400; this protocol's own legacy
        // device endpoint returns the identical object as 200. A client that
        // switched on the status would work against exactly one of them.
        ScriptedServer legacy = new ScriptedServer().answering(200, "{\"error\":\"authorization_pending\"}");

        assertInstanceOf(OAuth.DevicePollResult.Pending.class, poll(legacy, Duration.ofSeconds(3)));
    }

    @Test
    void widensTheIntervalWhenToldToSlowDown() {
        ScriptedServer server = new ScriptedServer().answering(400, "{\"error\":\"slow_down\"}");

        OAuth.DevicePollResult result = poll(server, Duration.ofSeconds(3));

        assertEquals(
                Duration.ofSeconds(8),
                assertInstanceOf(OAuth.DevicePollResult.Pending.class, result).retryAfter());
    }

    @Test
    void keepsWideningTheIntervalEachTimeItIsToldTo() {
        // RFC 8628 widens permanently, so the caller feeds the last interval back
        // in. A client that kept sending the original would be told to slow down
        // for as long as it kept asking.
        ScriptedServer server =
                new ScriptedServer()
                        .answering(400, "{\"error\":\"slow_down\"}")
                        .answering(400, "{\"error\":\"slow_down\"}");

        Duration first =
                assertInstanceOf(OAuth.DevicePollResult.Pending.class, poll(server, Duration.ofSeconds(3)))
                        .retryAfter();
        Duration second =
                assertInstanceOf(OAuth.DevicePollResult.Pending.class, poll(server, first)).retryAfter();

        assertEquals(Duration.ofSeconds(8), first);
        assertEquals(Duration.ofSeconds(13), second);
    }

    @Test
    void stopsWhenThePersonRefuses() {
        ScriptedServer server = new ScriptedServer().answering(400, "{\"error\":\"access_denied\"}");

        AuruException error = assertThrows(AuruException.class, () -> poll(server, Duration.ofSeconds(3)));

        assertEquals(ErrorCode.UNAUTHORIZED, error.code());
        assertTrue(error.getMessage().contains("refused"), error.getMessage());
    }

    @Test
    void stopsWhenTheCodeExpires() {
        ScriptedServer server = new ScriptedServer().answering(400, "{\"error\":\"expired_token\"}");

        AuruException error = assertThrows(AuruException.class, () -> poll(server, Duration.ofSeconds(3)));

        assertEquals(ErrorCode.UNAUTHORIZED, error.code());
        assertTrue(error.getMessage().contains("expired"), error.getMessage());
    }

    @Test
    void reportsAnUnrecognisedErrorRatherThanPollingThroughIt() {
        ScriptedServer server =
                new ScriptedServer()
                        .answering(
                                400,
                                "{\"error\":\"invalid_client\","
                                        + "\"error_description\":\"this client is not allowed here\"}");

        AuruException error = assertThrows(AuruException.class, () -> poll(server, Duration.ofSeconds(3)));

        assertTrue(error.getMessage().contains("this client is not allowed here"), error.getMessage());
    }

    // ── The whole loop ───────────────────────────────────────────────────────

    @Test
    void waitsThroughPendingPollsAndReturnsTheToken() {
        ScriptedServer server =
                new ScriptedServer()
                        .answering(400, "{\"error\":\"authorization_pending\"}")
                        .answering(400, "{\"error\":\"authorization_pending\"}")
                        .answering(200, TOKEN_RESPONSE);

        OAuth.DeviceAuthorization brisk =
                new OAuth.DeviceAuthorization(
                        "a-device-code",
                        "ABCD-1234",
                        "https://identity.example.com/activate",
                        Optional.empty(),
                        Duration.ofSeconds(600),
                        Duration.ofMillis(10));

        OAuth.AccessToken token =
                OAuth.completeDeviceAuthorization(metadata(), client(), brisk, server);

        assertEquals("an-access-token", token.token());
    }

    @Test
    void givesUpOnceTheCodeCanNoLongerBeApproved() {
        // Nothing is scripted: the deadline has to stop it before it asks.
        OAuth.DeviceAuthorization expired =
                new OAuth.DeviceAuthorization(
                        "a-device-code",
                        "ABCD-1234",
                        "https://identity.example.com/activate",
                        Optional.empty(),
                        Duration.ZERO,
                        Duration.ofMillis(10));

        AuruException error =
                assertThrows(
                        AuruException.class,
                        () ->
                                OAuth.completeDeviceAuthorization(
                                        metadata(), client(), expired, new ScriptedServer()));

        assertTrue(error.getMessage().contains("expired"), error.getMessage());
    }

    @Test
    void discardsTheRefreshTokenUnlessItIsAskedFor() {
        // Same rule the authorization-code flow follows: a caller handed a
        // refresh token will eventually store it somewhere worse than nowhere.
        ScriptedServer server = new ScriptedServer().answering(200, TOKEN_RESPONSE);
        OAuth.DeviceAuthorization brisk =
                new OAuth.DeviceAuthorization(
                        "a-device-code",
                        "ABCD-1234",
                        "https://identity.example.com/activate",
                        Optional.empty(),
                        Duration.ofSeconds(600),
                        Duration.ofMillis(10));

        OAuth.AccessToken token = OAuth.completeDeviceAuthorization(metadata(), client(), brisk, server);

        // The type is the guarantee: an AccessToken has nowhere to put one.
        assertEquals("an-access-token", token.token());
        assertEquals("Bearer", token.tokenType());
    }
}

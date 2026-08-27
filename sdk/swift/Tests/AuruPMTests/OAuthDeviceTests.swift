import Foundation
import XCTest

@testable import AuruPM

/// The RFC 8628 device authorization grant.
///
/// These run against a scripted authorization server rather than a real one,
/// which is the exception to this suite's usual rule. `TestProvider` exists
/// because mocked HTTP only proves a client agrees with the test's idea of the
/// protocol — but `auru-pm-server` is a resource server and issues no device
/// codes at all, so there is no reference implementation here to interoperate
/// with. What is being pinned down is this client's half: which parameters it
/// sends, and how it reads each of the four RFC 8628 error codes back.
///
/// The polling rules are where a device flow actually goes wrong. A client that
/// treats `slow_down` as a failure locks the person out; one that ignores it
/// hammers the server into rate-limiting them; one that reads the pending signal
/// off the HTTP status rather than the body works against one provider and not
/// the next.
final class OAuthDeviceTests: XCTestCase {

    private let issuer = "https://identity.example.com"

    /// Hands back queued responses in order, one per request.
    private final class Script: @unchecked Sendable {
        private let lock = NSLock()
        private var queued: [HTTPResponse]

        init(_ queued: [HTTPResponse]) { self.queued = queued }

        var handler: @Sendable (HTTPRequest) -> HTTPResponse {
            { [self] _ in
                lock.withLock {
                    guard !queued.isEmpty else {
                        return StubTransport.raw(500, "the client made more requests than scripted")
                    }
                    return queued.removeFirst()
                }
            }
        }
    }

    private func scripted(_ responses: HTTPResponse...) -> StubTransport {
        StubTransport(handler: Script(responses).handler)
    }

    private var metadata: OAuth.ServerMetadata {
        OAuth.ServerMetadata(
            issuer: issuer,
            authorizationEndpoint: issuer + "/authorize",
            tokenEndpoint: issuer + "/token",
            codeChallengeMethodsSupported: ["S256"],
            deviceAuthorizationEndpoint: issuer + "/device")
    }

    private var client: OAuthClient {
        OAuthClient(
            kind: .native, clientID: "auru-pm-desktop",
            redirectURI: "http://127.0.0.1:43827/oauth/callback",
            flows: ["authorization_code_pkce", "device_authorization"])
    }

    private var authorization: OAuth.DeviceAuthorization {
        OAuth.DeviceAuthorization(
            deviceCode: "a-device-code", userCode: "ABCD-1234",
            verificationURI: "https://identity.example.com/activate",
            expiresIn: 600, interval: 3)
    }

    private let deviceResponse = """
        {"device_code":"a-device-code","user_code":"ABCD-1234",\
        "verification_uri":"https://identity.example.com/activate",\
        "verification_uri_complete":"https://identity.example.com/activate?user_code=ABCD-1234",\
        "expires_in":600,"interval":3}
        """

    private let tokenResponse = """
        {"access_token":"an-access-token","refresh_token":"a-refresh-token",\
        "token_type":"Bearer","expires_in":3600,"scope":"openid"}
        """

    /// The form body of the last request the transport saw.
    private func lastForm(_ transport: StubTransport) -> [String: String] {
        let body = String(decoding: transport.requests.last?.body ?? Data(), as: UTF8.self)
        var fields: [String: String] = [:]
        for pair in body.split(separator: "&") {
            guard let equals = pair.firstIndex(of: "=") else { continue }
            let name = String(pair[..<equals]).removingPercentEncoding ?? ""
            let value = String(pair[pair.index(after: equals)...])
            fields[name] = value.removingPercentEncoding ?? value
        }
        return fields
    }

    // MARK: - Discovery

    func testDiscoveryCarriesTheDeviceEndpointWhenThereIsOne() async throws {
        let document = """
            {"issuer":"\(issuer)","authorization_endpoint":"\(issuer)/authorize",\
            "token_endpoint":"\(issuer)/token",\
            "device_authorization_endpoint":"\(issuer)/device"}
            """
        let transport = StubTransport { _ in StubTransport.json(200, document) }

        let discovered = try await OAuth.discover(issuer: issuer, transport: transport)

        XCTAssertEqual(discovered.deviceAuthorizationEndpoint, issuer + "/device")
    }

    func testDiscoveryLeavesTheDeviceEndpointNilWhenThereIsNone() async throws {
        let document = """
            {"issuer":"\(issuer)","authorization_endpoint":"\(issuer)/authorize",\
            "token_endpoint":"\(issuer)/token"}
            """
        let transport = StubTransport { _ in StubTransport.json(200, document) }

        let discovered = try await OAuth.discover(issuer: issuer, transport: transport)

        XCTAssertNil(discovered.deviceAuthorizationEndpoint)
    }

    // MARK: - Asking for a code

    func testAsksTheDeviceEndpointForACodeAndReadsItBack() async throws {
        let transport = scripted(StubTransport.json(200, deviceResponse))

        let granted = try await OAuth.beginDeviceAuthorization(
            metadata: metadata, client: client, scope: "openid", transport: transport)

        XCTAssertEqual(transport.requests.last?.url, issuer + "/device")
        XCTAssertEqual(transport.requests.last?.method, "POST")
        XCTAssertEqual(lastForm(transport)["client_id"], "auru-pm-desktop")
        XCTAssertEqual(lastForm(transport)["scope"], "openid")

        XCTAssertEqual(granted.deviceCode, "a-device-code")
        XCTAssertEqual(granted.userCode, "ABCD-1234")
        XCTAssertEqual(granted.verificationURI, "https://identity.example.com/activate")
        XCTAssertEqual(
            granted.verificationURIComplete,
            "https://identity.example.com/activate?user_code=ABCD-1234")
        XCTAssertEqual(granted.expiresIn, 600)
        XCTAssertEqual(granted.interval, 3)
    }

    func testSendsNoRedirectURIAndNoPKCEVerifier() async throws {
        // The whole reason this flow works on a phone: nothing here depends on a
        // redirect the identity provider had to be told to allow.
        let transport = scripted(StubTransport.json(200, deviceResponse))

        _ = try await OAuth.beginDeviceAuthorization(
            metadata: metadata, client: client, scope: "openid", transport: transport)

        XCTAssertNil(lastForm(transport)["redirect_uri"])
        XCTAssertNil(lastForm(transport)["code_challenge"])
    }

    func testFallsBackToTheSpecsOwnDefaultsWhenTheServerOmitsTheTimings() async throws {
        let sparse = """
            {"device_code":"a-device-code","user_code":"ABCD-1234",\
            "verification_uri":"https://identity.example.com/activate"}
            """
        let transport = scripted(StubTransport.json(200, sparse))

        let granted = try await OAuth.beginDeviceAuthorization(
            metadata: metadata, client: client, scope: "openid", transport: transport)

        XCTAssertEqual(granted.expiresIn, 300)
        XCTAssertEqual(granted.interval, 5)
    }

    func testRefusesAProviderThatPublishesNoDeviceEndpoint() async throws {
        let withoutDevice = OAuth.ServerMetadata(
            issuer: issuer, authorizationEndpoint: issuer + "/authorize",
            tokenEndpoint: issuer + "/token")
        let transport = scripted()

        do {
            _ = try await OAuth.beginDeviceAuthorization(
                metadata: withoutDevice, client: client, scope: "openid", transport: transport)
            XCTFail("expected the missing endpoint to be refused")
        } catch let error as AuruError {
            XCTAssertEqual(error.code, .unsupported)
            XCTAssertTrue(
                error.message.contains("publishes no device authorization endpoint"), error.message)
        }
    }

    func testRefusesAClientNotRegisteredForTheFlow() async throws {
        // Registration is per flow. A client id allowed to run PKCE is not
        // thereby allowed to run this.
        let pkceOnly = OAuthClient(
            kind: .native, clientID: "auru-pm-desktop",
            redirectURI: "http://127.0.0.1:43827/oauth/callback",
            flows: ["authorization_code_pkce"])

        do {
            _ = try await OAuth.beginDeviceAuthorization(
                metadata: metadata, client: pkceOnly, scope: "openid", transport: scripted())
            XCTFail("expected the unregistered flow to be refused")
        } catch let error as AuruError {
            XCTAssertEqual(error.code, .unsupported)
            XCTAssertTrue(error.message.contains("device_authorization"), error.message)
        }
    }

    func testRefusesADeviceResponseMissingTheCodeItMustPollWith() async throws {
        let incomplete = """
            {"user_code":"ABCD-1234",\
            "verification_uri":"https://identity.example.com/activate"}
            """
        let transport = scripted(StubTransport.json(200, incomplete))

        do {
            _ = try await OAuth.beginDeviceAuthorization(
                metadata: metadata, client: client, scope: "openid", transport: transport)
            XCTFail("expected the missing device_code to be refused")
        } catch let error as AuruError {
            XCTAssertTrue(error.message.contains("device_code"), error.message)
        }
    }

    // MARK: - Polling

    private func poll(_ transport: StubTransport, interval: TimeInterval = 3) async throws
        -> OAuth.DevicePollResult
    {
        try await OAuth.pollDeviceAuthorization(
            metadata: metadata, client: client, authorization: authorization, interval: interval,
            transport: transport)
    }

    func testPollsTheTokenEndpointWithTheDeviceCodeGrant() async throws {
        let transport = scripted(StubTransport.json(200, tokenResponse))

        _ = try await poll(transport)

        XCTAssertEqual(transport.requests.last?.url, issuer + "/token")
        XCTAssertEqual(
            lastForm(transport)["grant_type"], "urn:ietf:params:oauth:grant-type:device_code")
        XCTAssertEqual(lastForm(transport)["device_code"], "a-device-code")
        XCTAssertEqual(lastForm(transport)["client_id"], "auru-pm-desktop")
    }

    func testReadsTheTokenOnceTheCodeIsApproved() async throws {
        let transport = scripted(StubTransport.json(200, tokenResponse))

        guard case .authorized(let token) = try await poll(transport) else {
            return XCTFail("expected the poll to be authorized")
        }
        XCTAssertEqual(token.access.token, "an-access-token")
        XCTAssertEqual(token.refreshToken, "a-refresh-token")
        XCTAssertEqual(token.access.expiresIn, 3600)
    }

    func testKeepsWaitingWhileNobodyHasApprovedItYet() async throws {
        let transport = scripted(StubTransport.json(400, #"{"error":"authorization_pending"}"#))

        let result = try await poll(transport)
        XCTAssertEqual(result, .pending(retryAfter: 3))
    }

    func testReadsThePendingSignalFromTheBodyWhateverStatusCarriesIt() async throws {
        // OAuth 2.0 returns an error object as 400; this protocol's own legacy
        // device endpoint returns the identical object as 200. A client that
        // switched on the status would work against exactly one of them.
        let legacy = scripted(StubTransport.json(200, #"{"error":"authorization_pending"}"#))

        let result = try await poll(legacy)
        XCTAssertEqual(result, .pending(retryAfter: 3))
    }

    func testWidensTheIntervalWhenToldToSlowDown() async throws {
        let transport = scripted(StubTransport.json(400, #"{"error":"slow_down"}"#))

        let result = try await poll(transport)
        XCTAssertEqual(result, .pending(retryAfter: 8))
    }

    func testKeepsWideningTheIntervalEachTimeItIsToldTo() async throws {
        // RFC 8628 widens permanently, so the caller feeds the last interval back
        // in. A client that kept sending the original would be told to slow down
        // for as long as it kept asking.
        let transport = scripted(
            StubTransport.json(400, #"{"error":"slow_down"}"#),
            StubTransport.json(400, #"{"error":"slow_down"}"#))

        guard case .pending(let first) = try await poll(transport) else {
            return XCTFail("expected the first poll to be pending")
        }
        guard case .pending(let second) = try await poll(transport, interval: first) else {
            return XCTFail("expected the second poll to be pending")
        }
        XCTAssertEqual(first, 8)
        XCTAssertEqual(second, 13)
    }

    func testStopsWhenThePersonRefuses() async throws {
        let transport = scripted(StubTransport.json(400, #"{"error":"access_denied"}"#))

        do {
            _ = try await poll(transport)
            XCTFail("expected a refusal to stop the flow")
        } catch let error as AuruError {
            XCTAssertEqual(error.code, .unauthorized)
            XCTAssertTrue(error.message.contains("refused"), error.message)
        }
    }

    func testStopsWhenTheCodeExpires() async throws {
        let transport = scripted(StubTransport.json(400, #"{"error":"expired_token"}"#))

        do {
            _ = try await poll(transport)
            XCTFail("expected an expired code to stop the flow")
        } catch let error as AuruError {
            XCTAssertEqual(error.code, .unauthorized)
            XCTAssertTrue(error.message.contains("expired"), error.message)
        }
    }

    func testReportsAnUnrecognisedErrorRatherThanPollingThroughIt() async throws {
        let transport = scripted(
            StubTransport.json(
                400,
                #"{"error":"invalid_client","error_description":"this client is not allowed here"}"#
            ))

        do {
            _ = try await poll(transport)
            XCTFail("expected an unrecognised error to stop the flow")
        } catch let error as AuruError {
            XCTAssertTrue(error.message.contains("this client is not allowed here"), error.message)
        }
    }

    // MARK: - The whole loop

    /// The same authorization, but polled fast enough for a test to sit through.
    private var brisk: OAuth.DeviceAuthorization {
        OAuth.DeviceAuthorization(
            deviceCode: "a-device-code", userCode: "ABCD-1234",
            verificationURI: "https://identity.example.com/activate",
            expiresIn: 600, interval: 0.01)
    }

    func testWaitsThroughPendingPollsAndReturnsTheToken() async throws {
        let transport = scripted(
            StubTransport.json(400, #"{"error":"authorization_pending"}"#),
            StubTransport.json(400, #"{"error":"authorization_pending"}"#),
            StubTransport.json(200, tokenResponse))

        let token = try await OAuth.completeDeviceAuthorization(
            metadata: metadata, client: client, authorization: brisk, transport: transport)

        XCTAssertEqual(token.token, "an-access-token")
        XCTAssertEqual(transport.requests.count, 3)
    }

    func testGivesUpOnceTheCodeCanNoLongerBeApproved() async throws {
        // Nothing is scripted: the deadline has to stop it before it asks.
        let expired = OAuth.DeviceAuthorization(
            deviceCode: "a-device-code", userCode: "ABCD-1234",
            verificationURI: "https://identity.example.com/activate",
            expiresIn: 0, interval: 0.01)
        let transport = scripted()

        do {
            _ = try await OAuth.completeDeviceAuthorization(
                metadata: metadata, client: client, authorization: expired, transport: transport)
            XCTFail("expected the deadline to end the wait")
        } catch let error as AuruError {
            XCTAssertTrue(error.message.contains("expired"), error.message)
            XCTAssertTrue(transport.requests.isEmpty, "it should not have polled at all")
        }
    }

    func testACancelledWaitStopsPolling() async throws {
        // A person who backs out of the sign-in screen should not leave a task
        // quietly polling an identity provider for the next ten minutes.
        let transport = scripted(
            StubTransport.json(400, #"{"error":"authorization_pending"}"#))
        let slow = OAuth.DeviceAuthorization(
            deviceCode: "a-device-code", userCode: "ABCD-1234",
            verificationURI: "https://identity.example.com/activate",
            expiresIn: 600, interval: 30)

        let task = Task {
            try await OAuth.completeDeviceAuthorization(
                metadata: metadata, client: client, authorization: slow, transport: transport)
        }
        task.cancel()

        do {
            _ = try await task.value
            XCTFail("expected cancellation to end the wait")
        } catch is CancellationError {
            XCTAssertTrue(transport.requests.isEmpty, "it should not have polled at all")
        }
    }

    // MARK: - Refresh (RFC 6749 §6)

    func testRefreshSendsTheGrantTheProviderExpects() async throws {
        let transport = scripted(StubTransport.json(200, tokenResponse))

        _ = try await OAuth.refresh(
            metadata: metadata, client: client, refreshToken: "an-old-refresh-token",
            transport: transport)

        XCTAssertEqual(transport.requests.last?.url, issuer + "/token")
        let form = lastForm(transport)
        XCTAssertEqual(form["grant_type"], "refresh_token")
        XCTAssertEqual(form["refresh_token"], "an-old-refresh-token")
        XCTAssertEqual(form["client_id"], "auru-pm-desktop")
        // A public client has no secret to send, and sending one would mean it
        // had been shipped inside the app.
        XCTAssertNil(form["client_secret"])
        // Omitted rather than sent empty: an empty scope is not "everything".
        XCTAssertNil(form["scope"])
    }

    func testRefreshCarriesANarrowedScopeWhenAskedFor() async throws {
        let transport = scripted(StubTransport.json(200, tokenResponse))

        _ = try await OAuth.refresh(
            metadata: metadata, client: client, refreshToken: "a-refresh-token", scope: "openid",
            transport: transport)

        XCTAssertEqual(lastForm(transport)["scope"], "openid")
    }

    func testRefreshReturnsTheRotatedTokenSoTheCallerStoresTheNewOne() async throws {
        let rotated = """
            {"access_token":"a-new-access-token","refresh_token":"a-rotated-refresh-token",\
            "token_type":"Bearer","expires_in":3600,"scope":"openid"}
            """
        let transport = scripted(StubTransport.json(200, rotated))

        let token = try await OAuth.refresh(
            metadata: metadata, client: client, refreshToken: "the-old-one", transport: transport)

        XCTAssertEqual(token.access.token, "a-new-access-token")
        // Storing the old one after a rotation is how a client gets its whole
        // session revoked on the next refresh: the server reads a retired token
        // as evidence that a copy leaked.
        XCTAssertEqual(token.refreshToken, "a-rotated-refresh-token")
    }

    func testRefreshKeepsThePresentedTokenWhenTheProviderDoesNotRotate() async throws {
        let unrotated = """
            {"access_token":"a-new-access-token","token_type":"Bearer","expires_in":3600}
            """
        let transport = scripted(StubTransport.json(200, unrotated))

        let token = try await OAuth.refresh(
            metadata: metadata, client: client, refreshToken: "still-current",
            transport: transport)

        // The caller stores whatever comes back, so a provider that does not
        // rotate must not leave it storing nothing and signing in again.
        XCTAssertEqual(token.refreshToken, "still-current")
    }

    func testARevokedRefreshTokenIsUnauthorizedRatherThanRetryable() async throws {
        let transport = scripted(
            StubTransport.json(
                400,
                #"{"error":"invalid_grant","error_description":"this refresh token has already been used"}"#
            ))

        do {
            _ = try await OAuth.refresh(
                metadata: metadata, client: client, refreshToken: "a-reused-token",
                transport: transport)
            XCTFail("expected a rejected refresh token to throw")
        } catch let error as AuruError {
            // Terminal, not transient. A caller that retries this will keep
            // failing; the only way forward is to sign in again.
            XCTAssertEqual(error.code, .unauthorized)
            XCTAssertTrue(
                error.message.contains("already been used"),
                "the provider's own reason should survive: \(error.message)")
        }
    }
}

import Foundation
import XCTest

@testable import AuruPM

/// PKCE, discovery, and the token rules.
///
/// Most of what matters here is what the flow refuses. A client that accepts a
/// discovery document naming someone else's token endpoint, or that hands a
/// refresh token to code with nowhere safe to put it, fails in a way its users
/// never see until it matters.
final class OAuthTests: XCTestCase {

    private let issuer = "https://identity.example.com"

    private var metadata: OAuth.ServerMetadata {
        OAuth.ServerMetadata(
            issuer: issuer,
            authorizationEndpoint: issuer + "/authorize",
            tokenEndpoint: issuer + "/token",
            codeChallengeMethodsSupported: ["S256"])
    }

    private var browserClient: OAuthClient {
        OAuthClient(
            kind: .browser, clientID: "dashboard",
            redirectURI: "https://dashboard.example.com/oauth/callback",
            flows: ["authorization_code_pkce"])
    }

    private var discoveryDocument: String {
        """
        {"issuer":"\(issuer)","authorization_endpoint":"\(issuer)/authorize",\
        "token_endpoint":"\(issuer)/token","code_challenge_methods_supported":["S256"]}
        """
    }

    private let tokenResponse = """
        {"access_token":"an-access-token","refresh_token":"a-refresh-token",\
        "token_type":"Bearer","expires_in":3600,"scope":"openid"}
        """

    // MARK: - SHA-256, which PKCE rests on

    func testSHA256MatchesThePublishedFIPSVectors() {
        func hex(_ bytes: [UInt8]) -> String {
            bytes.map { String(format: "%02x", $0) }.joined()
        }
        XCTAssertEqual(
            hex(SHA256.hash([])),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        XCTAssertEqual(
            hex(SHA256.hash(Array("abc".utf8))),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        XCTAssertEqual(
            hex(SHA256.hash(Array("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq".utf8))),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1")
        // Crosses the padding boundary, where a length-handling error hides.
        XCTAssertEqual(
            hex(SHA256.hash(Array(String(repeating: "a", count: 64).utf8))),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb")
    }

    // MARK: - Choosing a client

    func testFindsEachRegisteredClientKind() throws {
        let json = try JSON.parse("""
            {"issuer":"\(issuer)","audience":"auru-pm","required_scope":"openid","clients":[\
            {"kind":"native","client_id":"desktop",\
            "redirect_uri":"http://127.0.0.1:43827/oauth/callback",\
            "flows":["authorization_code_pkce"]},\
            {"kind":"browser","client_id":"dashboard",\
            "redirect_uri":"https://dashboard.example.com/cb",\
            "flows":["authorization_code_pkce"]}]}
            """)
        let configuration = OAuthConfiguration(json: json)
        XCTAssertEqual(configuration.client(.native)?.clientID, "desktop")
        XCTAssertEqual(configuration.client(.browser)?.clientID, "dashboard")
    }

    func testReadsThePreClientsSingularFieldsAsTheNativeClient() throws {
        // What every provider written before `clients` existed sends.
        let json = try JSON.parse("""
            {"issuer":"\(issuer)","audience":"auru-pm","required_scope":"openid",\
            "client_id":"desktop","redirect_uri":"http://127.0.0.1:43827/oauth/callback",\
            "flows":["authorization_code_pkce"]}
            """)
        let configuration = OAuthConfiguration(json: json)
        XCTAssertEqual(configuration.client(.native)?.clientID, "desktop")
        XCTAssertNil(configuration.client(.browser))
    }

    // MARK: - Discovery

    func testAcceptsADocumentWhoseIssuerMatchesExactly() async throws {
        let document = discoveryDocument
        let transport = StubTransport { _ in StubTransport.json(200, document) }
        let found = try await OAuth.discover(issuer: issuer, transport: transport)
        XCTAssertEqual(found.tokenEndpoint, issuer + "/token")
    }

    func testRefusesADocumentClaimingADifferentIssuer() async throws {
        // A provider that could name someone else's issuer could send a user's
        // credentials there.
        let transport = StubTransport { _ in
            StubTransport.json(
                200,
                #"{"issuer":"https://attacker.example.com","authorization_endpoint":"https://attacker.example.com/a","token_endpoint":"https://attacker.example.com/t"}"#
            )
        }
        do {
            _ = try await OAuth.discover(issuer: issuer, transport: transport)
            XCTFail("expected a failure")
        } catch let error as AuruError {
            XCTAssertTrue(error.message.contains("claims issuer"))
        }
    }

    func testRefusesADocumentMissingTheEndpointsPKCENeeds() async throws {
        let issuerValue = issuer
        let transport = StubTransport { _ in
            StubTransport.json(200, "{\"issuer\":\"\(issuerValue)\"}")
        }
        do {
            _ = try await OAuth.discover(issuer: issuer, transport: transport)
            XCTFail("expected a failure")
        } catch let error as AuruError {
            XCTAssertTrue(error.message.contains("omits the endpoints"))
        }
    }

    func testReportsAnIssuerItCannotReach() async throws {
        let transport = StubTransport { _ in StubTransport.raw(404, "") }
        do {
            _ = try await OAuth.discover(issuer: issuer, transport: transport)
            XCTFail("expected a failure")
        } catch let error as AuruError {
            XCTAssertTrue(error.message.contains("cannot discover"))
        }
    }

    // MARK: - Beginning authorization

    func testBuildsAnS256ChallengeAndAFreshState() throws {
        let request = try OAuth.beginAuthorization(
            metadata: metadata, client: browserClient, scope: "openid")
        let parameters = OAuth.query(of: request.url)

        XCTAssertTrue(request.url.hasPrefix(issuer + "/authorize?"))
        XCTAssertEqual(parameters["response_type"], "code")
        XCTAssertEqual(parameters["client_id"], "dashboard")
        XCTAssertEqual(parameters["code_challenge_method"], "S256")
        XCTAssertEqual(parameters["state"], request.state)

        // The challenge must actually be SHA-256 of the verifier, not a copy.
        let expected = Data(SHA256.hash(Array(request.codeVerifier.utf8)))
            .base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
        XCTAssertEqual(parameters["code_challenge"], expected)
        XCTAssertNotEqual(parameters["code_challenge"], request.codeVerifier)
    }

    func testGivesEveryRequestItsOwnVerifierAndState() throws {
        let first = try OAuth.beginAuthorization(
            metadata: metadata, client: browserClient, scope: "openid")
        let second = try OAuth.beginAuthorization(
            metadata: metadata, client: browserClient, scope: "openid")
        XCTAssertNotEqual(first.codeVerifier, second.codeVerifier)
        XCTAssertNotEqual(first.state, second.state)
    }

    func testRefusesAProviderThatDoesNotOfferS256() {
        var plainOnly = metadata
        plainOnly.codeChallengeMethodsSupported = ["plain"]
        XCTAssertThrowsError(
            try OAuth.beginAuthorization(metadata: plainOnly, client: browserClient, scope: "openid")
        ) { error in
            XCTAssertTrue("\(error)".contains("PKCE S256"))
        }
    }

    func testRefusesAClientNotRegisteredForTheFlow() {
        var deviceOnly = browserClient
        deviceOnly.flows = ["device_authorization"]
        XCTAssertThrowsError(
            try OAuth.beginAuthorization(metadata: metadata, client: deviceOnly, scope: "openid")
        ) { error in
            XCTAssertTrue("\(error)".contains("authorization_code_pkce"))
        }
    }

    func testCarriesExtraAuthorizationParameters() throws {
        let request = try OAuth.beginAuthorization(
            metadata: metadata, client: browserClient, scope: "openid",
            extraParameters: ["prompt": "consent"])
        XCTAssertEqual(OAuth.query(of: request.url)["prompt"], "consent")
    }

    // MARK: - The redirect

    func testRefusesARedirectWhoseStateDoesNotMatch() throws {
        let request = try OAuth.beginAuthorization(
            metadata: metadata, client: browserClient, scope: "openid")
        XCTAssertThrowsError(
            try request.code(from: "https://dashboard.example.com/cb?code=abc&state=other")
        ) { error in
            XCTAssertTrue("\(error)".contains("state does not match"))
        }
    }

    func testSurfacesAnErrorReturnedInsteadOfACode() throws {
        let request = try OAuth.beginAuthorization(
            metadata: metadata, client: browserClient, scope: "openid")
        XCTAssertThrowsError(
            try request.code(
                from:
                    "https://dashboard.example.com/cb?error=access_denied&error_description=user+said+no&state="
                    + request.state)
        ) { error in
            XCTAssertTrue("\(error)".contains("user said no"))
        }
    }

    func testPullsTheCodeOutOfAMatchingRedirect() throws {
        let request = try OAuth.beginAuthorization(
            metadata: metadata, client: browserClient, scope: "openid")
        XCTAssertEqual(
            try request.code(
                from: "https://dashboard.example.com/cb?code=the-code&state=" + request.state),
            "the-code")
    }

    // MARK: - Completing authorization

    func testDiscardsTheRefreshTokenByDefault() async throws {
        let response = tokenResponse
        let transport = StubTransport { _ in StubTransport.json(200, response) }
        let request = try OAuth.beginAuthorization(
            metadata: metadata, client: browserClient, scope: "openid")

        let token = try await OAuth.completeAuthorization(
            metadata: metadata, client: browserClient, request: request, code: "the-code",
            transport: transport)

        XCTAssertEqual(token.token, "an-access-token")
        XCTAssertEqual(token.expiresIn, 3600)
        // There is no member that could return it, which is the point.
    }

    func testReturnsTheRefreshTokenOnlyWhenAskedForExplicitly() async throws {
        let response = tokenResponse
        let transport = StubTransport { _ in StubTransport.json(200, response) }
        let request = try OAuth.beginAuthorization(
            metadata: metadata, client: browserClient, scope: "openid")
        let token = try await OAuth.completeAuthorizationWithRefresh(
            metadata: metadata, client: browserClient, request: request, code: "the-code",
            transport: transport)
        XCTAssertEqual(token.refreshToken, "a-refresh-token")
    }

    func testSendsTheVerifierAndNoClientSecret() async throws {
        let response = tokenResponse
        let transport = StubTransport { _ in StubTransport.json(200, response) }
        let request = try OAuth.beginAuthorization(
            metadata: metadata, client: browserClient, scope: "openid")
        _ = try await OAuth.completeAuthorization(
            metadata: metadata, client: browserClient, request: request, code: "the-code",
            transport: transport)

        let body = String(decoding: transport.requests.last!.body!, as: UTF8.self)
        XCTAssertTrue(body.contains("grant_type=authorization_code"))
        XCTAssertTrue(body.contains("code_verifier=" + request.codeVerifier))
        XCTAssertTrue(body.contains("client_id=dashboard"))
        // A public client has no secret; sending one would mean it was shipped.
        XCTAssertFalse(body.contains("client_secret"))
    }

    func testReportsTheProvidersErrorDescriptionWhenExchangeFails() async throws {
        let transport = StubTransport { _ in
            StubTransport.json(
                400, #"{"error":"invalid_grant","error_description":"code already used"}"#)
        }
        let request = try OAuth.beginAuthorization(
            metadata: metadata, client: browserClient, scope: "openid")
        do {
            _ = try await OAuth.completeAuthorization(
                metadata: metadata, client: browserClient, request: request, code: "expired",
                transport: transport)
            XCTFail("expected a failure")
        } catch let error as AuruError {
            XCTAssertTrue(error.message.contains("code already used"))
        }
    }

    func testRefusesASuccessResponseCarryingNoAccessToken() async throws {
        let transport = StubTransport { _ in StubTransport.json(200, #"{"token_type":"Bearer"}"#) }
        let request = try OAuth.beginAuthorization(
            metadata: metadata, client: browserClient, scope: "openid")
        do {
            _ = try await OAuth.completeAuthorization(
                metadata: metadata, client: browserClient, request: request, code: "the-code",
                transport: transport)
            XCTFail("expected a failure")
        } catch let error as AuruError {
            XCTAssertTrue(error.message.contains("no access_token"))
        }
    }
}

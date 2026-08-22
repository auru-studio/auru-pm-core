import Foundation

/// OAuth 2.0 Authorization Code with PKCE, for providers that advertise it.
///
/// Two things here are deliberate and worth reading before changing them.
///
/// **Endpoints come from discovery, never from the provider's health
/// document.** A provider publishes only its issuer; ``discover(issuer:transport:)``
/// fetches that issuer's RFC 8414 or OpenID Connect metadata and rejects a
/// document whose `issuer` is not byte-identical to the one asked for. A
/// provider that could name its own token endpoint could name someone else's.
///
/// **The refresh token is discarded by default.** ``completeAuthorization(metadata:client:request:code:transport:)``
/// returns only a short-lived access token. A caller who is handed a refresh
/// token will eventually store it, and most places it could be stored are worse
/// than not having it at all. On iOS the right place is the Keychain, and
/// ``completeAuthorizationWithRefresh(metadata:client:request:code:transport:)``
/// is the explicit opt-in for callers using it.
public enum OAuth {

    /// The subset of authorization-server metadata this flow needs.
    public struct ServerMetadata: Sendable, Equatable {
        public var issuer: String
        public var authorizationEndpoint: String
        public var tokenEndpoint: String
        public var codeChallengeMethodsSupported: [String]

        public init(
            issuer: String, authorizationEndpoint: String, tokenEndpoint: String,
            codeChallengeMethodsSupported: [String] = []
        ) {
            self.issuer = issuer
            self.authorizationEndpoint = authorizationEndpoint
            self.tokenEndpoint = tokenEndpoint
            self.codeChallengeMethodsSupported = codeChallengeMethodsSupported
        }
    }

    /// An access token, held for as long as the caller keeps it.
    public struct AccessToken: Sendable, Equatable {
        public var token: String
        public var tokenType: String
        public var expiresIn: TimeInterval?
        public var scope: String?
    }

    /// An access token together with its refresh token.
    public struct RefreshableToken: Sendable, Equatable {
        public var access: AccessToken
        public var refreshToken: String?
    }

    /// A prepared authorization request.
    public struct AuthorizationRequest: Sendable, Equatable {
        /// Send the user here.
        public var url: String
        /// Echoed back on the redirect; a mismatch means the response is not ours.
        public var state: String
        /// The PKCE secret. Keep it in memory until the redirect returns.
        public var codeVerifier: String

        /// Check the redirect's `state` and pull out the code.
        public func code(from redirectURL: String) throws -> String {
            let parameters = OAuth.query(of: redirectURL)

            guard parameters["state"] == state else {
                throw AuruError(
                    code: .unauthorized,
                    message:
                        "authorization state does not match the request; the response is not ours")
            }
            if let error = parameters["error"] {
                throw AuruError(
                    code: .unauthorized,
                    message: "authorization failed: \(parameters["error_description"] ?? error)")
            }
            guard let code = parameters["code"] else {
                throw AuruError(code: .unauthorized, message: "the redirect carried no code")
            }
            return code
        }
    }

    /// Fetch and validate an issuer's authorization-server metadata.
    ///
    /// Tries OpenID Connect discovery, then RFC 8414.
    public static func discover(issuer: String, transport: any Transport) async throws
        -> ServerMetadata
    {
        var base = issuer
        while base.hasSuffix("/") { base.removeLast() }

        var lastProblem = "no discovery document"
        for suffix in ["/.well-known/openid-configuration", "/.well-known/oauth-authorization-server"] {
            let url = base + suffix
            let response: HTTPResponse
            do {
                response = try await transport.send(
                    HTTPRequest(
                        method: "GET", url: url,
                        headers: [(name: "accept", value: "application/json")]))
            } catch {
                lastProblem = "\(url): \(error)"
                continue
            }

            guard response.status == 200 else {
                lastProblem = "\(url): HTTP \(response.status)"
                continue
            }

            let document = try JSON.parse(response.body)
            let declared = document["issuer"]?.stringValue ?? ""
            guard declared == issuer else {
                throw AuruError(
                    code: .unauthorized,
                    message: "discovery at \(url) claims issuer \(declared), expected \(issuer)")
            }
            guard let authorization = document["authorization_endpoint"]?.stringValue,
                let token = document["token_endpoint"]?.stringValue
            else {
                throw AuruError(
                    code: .unsupported, message: "\(url) omits the endpoints PKCE needs")
            }

            return ServerMetadata(
                issuer: declared,
                authorizationEndpoint: authorization,
                tokenEndpoint: token,
                codeChallengeMethodsSupported: (document["code_challenge_methods_supported"]?
                    .arrayValue ?? []).compactMap(\.stringValue))
        }

        throw AuruError(
            code: .unauthorized, message: "cannot discover \(issuer): \(lastProblem)")
    }

    /// Build an authorization URL and the PKCE secret that completes it.
    ///
    /// Refuses a provider that does not advertise `S256`. A plain challenge is
    /// not a fallback worth having: the point of PKCE is that an intercepted
    /// authorization code is useless, which a plain challenge does not give you.
    public static func beginAuthorization(
        metadata: ServerMetadata, client: OAuthClient, scope: String,
        extraParameters: [String: String] = [:]
    ) throws -> AuthorizationRequest {
        let methods = metadata.codeChallengeMethodsSupported
        guard methods.isEmpty || methods.contains("S256") else {
            throw AuruError(
                code: .unsupported, message: "\(metadata.issuer) does not support PKCE S256")
        }
        guard client.flows.contains("authorization_code_pkce") else {
            throw AuruError(
                code: .unsupported,
                message: "client \(client.clientID) is not registered for authorization_code_pkce")
        }

        let codeVerifier = randomURLSafe(byteCount: 32)
        let state = randomURLSafe(byteCount: 16)
        let challenge = base64URL(SHA256.hash(Array(codeVerifier.utf8)))

        var parameters: [(String, String)] = [
            ("response_type", "code"),
            ("client_id", client.clientID),
            ("redirect_uri", client.redirectURI),
            ("scope", scope),
            ("state", state),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
        ]
        parameters.append(contentsOf: extraParameters.sorted { $0.key < $1.key }.map { ($0, $1) })

        let separator = metadata.authorizationEndpoint.contains("?") ? "&" : "?"
        return AuthorizationRequest(
            url: metadata.authorizationEndpoint + separator + form(parameters),
            state: state,
            codeVerifier: codeVerifier)
    }

    /// Exchange an authorization code for an access token.
    ///
    /// The refresh token, if the provider issued one, is discarded rather than
    /// returned. When the access token expires, run the flow again.
    public static func completeAuthorization(
        metadata: ServerMetadata, client: OAuthClient, request: AuthorizationRequest,
        code: String, transport: any Transport
    ) async throws -> AccessToken {
        try await completeAuthorizationWithRefresh(
            metadata: metadata, client: client, request: request, code: code, transport: transport
        ).access
    }

    /// Exchange an authorization code, keeping the refresh token.
    ///
    /// Only for callers with a real secret store — the Keychain on Apple
    /// platforms. A refresh token is a long-lived credential for the whole
    /// account; treat it the way you would treat a password.
    public static func completeAuthorizationWithRefresh(
        metadata: ServerMetadata, client: OAuthClient, request: AuthorizationRequest,
        code: String, transport: any Transport
    ) async throws -> RefreshableToken {
        // A public client has no secret. Sending one would mean it had been
        // shipped to wherever this code runs.
        let body = form([
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", client.redirectURI),
            ("client_id", client.clientID),
            ("code_verifier", request.codeVerifier),
        ])

        let response = try await transport.send(
            HTTPRequest(
                method: "POST", url: metadata.tokenEndpoint,
                headers: [
                    (name: "content-type", value: "application/x-www-form-urlencoded"),
                    (name: "accept", value: "application/json"),
                ],
                body: Data(body.utf8)))

        let payload = (try? JSON.parse(response.body)) ?? .emptyObject

        guard response.status / 100 == 2 else {
            let detail =
                payload["error_description"]?.stringValue ?? payload["error"]?.stringValue
                ?? "HTTP \(response.status)"
            throw AuruError(code: .unauthorized, message: "token exchange failed: \(detail)")
        }
        guard let token = payload["access_token"]?.stringValue else {
            throw AuruError(
                code: .unauthorized, message: "the token response carried no access_token")
        }

        return RefreshableToken(
            access: AccessToken(
                token: token,
                tokenType: payload["token_type"]?.stringValue ?? "bearer",
                expiresIn: payload["expires_in"]?.intValue.map(TimeInterval.init),
                scope: payload["scope"]?.stringValue),
            refreshToken: payload["refresh_token"]?.stringValue)
    }

    // MARK: - Helpers

    /// Cryptographically secure bytes.
    ///
    /// `SystemRandomNumberGenerator` is the portable answer: it draws from the
    /// platform CSPRNG on every target Swift supports. A predictable PKCE
    /// verifier is a PKCE flow that protects nothing.
    private static func randomURLSafe(byteCount: Int) -> String {
        var generator = SystemRandomNumberGenerator()
        let bytes = (0..<byteCount).map { _ in UInt8.random(in: 0...255, using: &generator) }
        return base64URL(bytes)
    }

    private static func base64URL(_ bytes: [UInt8]) -> String {
        Data(bytes).base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    private static func escape(_ value: String) -> String {
        value.addingPercentEncoding(
            withAllowedCharacters: .alphanumerics.union(.init(charactersIn: "-._~"))) ?? value
    }

    private static func form(_ parameters: [(String, String)]) -> String {
        parameters.map { "\(escape($0.0))=\(escape($0.1))" }.joined(separator: "&")
    }

    static func query(of url: String) -> [String: String] {
        guard let questionMark = url.firstIndex(of: "?") else { return [:] }
        let query = url[url.index(after: questionMark)...]

        var parameters: [String: String] = [:]
        for pair in query.split(separator: "&") {
            guard let equals = pair.firstIndex(of: "=") else { continue }
            let name = String(pair[..<equals]).removingPercentEncoding ?? String(pair[..<equals])
            let rawValue = String(pair[pair.index(after: equals)...]).replacingOccurrences(
                of: "+", with: " ")
            parameters[name] = rawValue.removingPercentEncoding ?? rawValue
        }
        return parameters
    }
}

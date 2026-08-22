import Foundation

/// A connected `auru-pm-v1` provider.
///
/// The endpoint is always supplied by the caller. There is no default host here
/// and no registry lookup: anyone can run a provider, and which one to trust is
/// the user's decision rather than this library's.
///
/// ```swift
/// let client = try await AuruClient.connect(
///     endpoint: "https://pm.example.com",
///     transport: URLSessionTransport(),
///     accessToken: token)
///
/// for version in try await client.project("user/night-drive").history(limit: 20) {
///     print(version.timestamp, version.message)
/// }
/// ```
///
/// An actor: the bearer token is mutable state shared by every call, and making
/// that safe by construction is cheaper than documenting a rule nobody reads.
public actor AuruClient {

    public let endpoint: String
    public let health: ProviderHealth

    private let transport: any Transport
    private var accessToken: String?

    private init(
        endpoint: String, health: ProviderHealth, transport: any Transport, accessToken: String?
    ) {
        self.endpoint = endpoint
        self.health = health
        self.transport = transport
        self.accessToken = accessToken
    }

    /// Read `/v1/health` and return a client bound to that provider.
    ///
    /// Health is read first because the protocol version, the auth methods and
    /// the capability set all have to be known before the first real call — one
    /// round trip up front is cheaper than discovering an unsupported endpoint
    /// halfway through a push.
    public static func connect(
        endpoint: String, transport: any Transport, accessToken: String? = nil
    ) async throws -> AuruClient {
        let normalized = try normalizeEndpoint(endpoint)

        let response = try await transport.send(
            HTTPRequest(
                method: "GET", url: normalized + "/v1/health",
                headers: [(name: "accept", value: "application/json")]))
        guard response.status / 100 == 2 else {
            throw AuruClient.failure(response)
        }

        let health = try ProviderHealth(json: try JSON.parse(response.body))
        guard health.protocolVersion == auruProtocolVersion else {
            throw AuruError(
                code: .unsupported,
                message:
                    "\(normalized) speaks \(health.protocolVersion); this client speaks \(auruProtocolVersion)"
            )
        }
        return AuruClient(
            endpoint: normalized, health: health, transport: transport, accessToken: accessToken)
    }

    public var capabilities: Capabilities { health.capabilities }

    /// Replace the bearer token, after a refresh for instance.
    public func setAccessToken(_ token: String?) {
        accessToken = token
    }

    /// Whether a token is held. Never exposes the token itself.
    public var isAuthenticated: Bool { accessToken != nil }

    /// The identity the provider derived from the current token.
    public func me() async throws -> AuthenticatedIdentity {
        try AuthenticatedIdentity(json: try await json("GET", "/v1/me"))
    }

    /// Projects visible to this account, newest first.
    public func listProjects() async throws -> [ProviderProject] {
        try require(capabilities.projectListing, "project_listing", "listing projects")
        let body = try await json("GET", "/v1/projects")
        return try (body["projects"]?.arrayValue ?? []).map { try ProviderProject(json: $0) }
    }

    /// Scope subsequent calls to one project handle.
    public func project(_ handle: String) throws -> ProjectClient {
        guard !handle.isEmpty else {
            throw AuruError(code: .badRequest, message: "a project handle must not be empty")
        }
        return ProjectClient(client: self, handle: handle)
    }

    // MARK: - Internals

    func require(_ advertised: Bool, _ capability: String, _ action: String) throws {
        guard advertised else {
            throw AuruError(
                code: .unsupported,
                message: "\(endpoint) does not support \(action) (capability \(capability))")
        }
    }

    func json(_ method: String, _ path: String, body: JSON? = nil) async throws -> JSON {
        let response = try await send(
            method, path, accept: "application/json",
            contentType: body == nil ? nil : "application/json",
            body: body.map { Data($0.jsonText().utf8) })
        return response.body.isEmpty ? .emptyObject : try JSON.parse(response.body)
    }

    func send(
        _ method: String, _ path: String, accept: String, contentType: String?, body: Data?
    ) async throws -> HTTPResponse {
        var headers: [(name: String, value: String)] = [(name: "accept", value: accept)]
        if let accessToken {
            headers.append((name: "authorization", value: "Bearer \(accessToken)"))
        }
        if let contentType {
            headers.append((name: "content-type", value: contentType))
        }

        let response = try await transport.send(
            HTTPRequest(method: method, url: endpoint + path, headers: headers, body: body))
        guard response.status / 100 == 2 else {
            throw AuruClient.failure(response)
        }
        return response
    }

    /// Turn a non-2xx response into the right error.
    ///
    /// A provider that invents a code outside the table, or answers with
    /// something that is not JSON at all, still has to produce a usable error
    /// here — a proxy returning an HTML 502 page is the ordinary case, not a
    /// hypothetical.
    static func failure(_ response: HTTPResponse) -> AuruError {
        let body = (try? JSON.parse(response.body)) ?? .emptyObject
        let rawCode = body["code"]?.stringValue

        if rawCode == "head_conflict" {
            return AuruError(
                code: .headConflict,
                message: "HEAD moved since it was last read",
                status: response.status,
                currentHead: body["current"]?.stringValue.flatMap { try? ContentHash(parsing: $0) })
        }

        let code = rawCode.flatMap(ErrorCode.init(rawValue:)) ?? .forStatus(response.status)
        let message =
            body["message"]?.stringValue ?? "HTTP \(response.status) from the provider"
        let retryAfter = response.header("retry-after").flatMap(TimeInterval.init)

        return AuruError(
            code: code, message: message, status: response.status, retryAfter: retryAfter)
    }

    private static func normalizeEndpoint(_ endpoint: String) throws -> String {
        guard let url = URL(string: endpoint), let scheme = url.scheme, let host = url.host else {
            throw AuruError(code: .badRequest, message: "endpoint is not a URL: \(endpoint)")
        }
        guard scheme == "http" || scheme == "https" else {
            throw AuruError(
                code: .badRequest, message: "endpoint must be http or https, got \(scheme)")
        }
        var normalized = "\(scheme)://\(host)"
        if let port = url.port { normalized += ":\(port)" }
        var path = url.path
        while path.hasSuffix("/") { path.removeLast() }
        return normalized + path
    }
}

/// Calls scoped to one project handle.
public struct ProjectClient: Sendable {

    private let client: AuruClient
    public let handle: String
    private let base: String

    init(client: AuruClient, handle: String) {
        self.client = client
        self.handle = handle
        self.base = "/v1/projects/" + ProjectClient.escape(handle)
    }

    private static func escape(_ value: String) -> String {
        value.addingPercentEncoding(withAllowedCharacters: .alphanumerics.union(.init(charactersIn: "-._~")))
            ?? value
    }

    /// Register this project's human-facing metadata.
    ///
    /// Also how the handle comes into existence: until a profile is registered
    /// every other project-scoped call answers `not_found`, blob upload
    /// included. Publish one before uploading anything.
    public func putProfile(_ profile: ProjectProfile) async throws {
        try await client.require(
            await client.capabilities.projectListing, "project_listing", "project profiles")
        _ = try await client.json("PUT", base, body: profile.json)
    }

    /// The current HEAD, `nil` on a project with no commits.
    public func head() async throws -> ContentHash? {
        let body = try await client.json("GET", base + "/head")
        guard let text = body["commit_id"]?.stringValue else { return nil }
        return try ContentHash(parsing: text)
    }

    /// Compare-and-swap HEAD from `from` to `to`.
    ///
    /// Pass `nil` for the initial publish. On failure the error is
    /// ``ErrorCode/headConflict`` with ``AuruError/currentHead`` set to the
    /// provider's actual HEAD, so a caller can rebase without asking again.
    public func advanceHead(from: ContentHash?, to: ContentHash) async throws {
        let body = JSON.object([
            JSONMember("from", from.map { JSON.string($0.description) } ?? .null),
            JSONMember("to", .string(to.description)),
        ])
        _ = try await client.json("POST", base + "/head", body: body)
    }

    /// Store a commit.
    ///
    /// The provider recomputes the id from the canonical encoding and rejects a
    /// mismatch — writing a commit you did not compute is an auth-equivalent
    /// failure, not a formatting slip. A ``Commit`` always carries a derived id,
    /// so this cannot be got wrong by construction.
    ///
    /// Idempotent: re-posting an existing id succeeds.
    @discardableResult
    public func putCommit(_ commit: Commit) async throws -> ContentHash {
        let body = try await client.json("POST", base + "/commits", body: commit.json)
        return try ContentHash(parsing: try body.string("id"))
    }

    public func getCommit(_ id: ContentHash) async throws -> Commit {
        try Commit(
            json: try await client.json(
                "GET", base + "/commits/" + ProjectClient.escape(id.description)))
    }

    /// History newest first.
    public func history(limit: Int32 = 0, before: ContentHash? = nil) async throws -> [CommitSummary] {
        try await history(HistoryRange(limit: limit, before: before))
    }

    /// History newest first.
    public func history(_ range: HistoryRange) async throws -> [CommitSummary] {
        var query: [String] = []
        if range.limit > 0 { query.append("limit=\(range.limit)") }
        if let before = range.before {
            query.append("before=" + ProjectClient.escape(before.description))
        }
        let path = base + "/history" + (query.isEmpty ? "" : "?" + query.joined(separator: "&"))
        let body = try await client.json("GET", path)
        return try (body["commits"]?.arrayValue ?? []).map { try CommitSummary(json: $0) }
    }

    /// Permanently move the oldest visible-history boundary. Irreversible.
    public func pruneHistory(_ request: RetentionRequest) async throws -> RetentionReport {
        try await client.require(
            await client.capabilities.historyRetention, "history_retention", "history retention")
        return try RetentionReport(
            json: try await client.json("POST", base + "/retention", body: request.json))
    }

    /// Which of `hashes` the provider already holds, parallel-indexed.
    public func hasBlobs(_ hashes: [ContentHash]) async throws -> [Bool] {
        guard !hashes.isEmpty else { return [] }
        let body = JSON.object([
            JSONMember("hashes", .array(hashes.map { .string($0.description) }))
        ])
        let response = try await client.json("POST", base + "/blobs/has", body: body)
        return (response["present"]?.arrayValue ?? []).map { $0.boolValue ?? false }
    }

    /// Upload a blob under its own hash.
    ///
    /// The hash is derived from the bytes rather than accepted from the caller,
    /// so an upload cannot be filed under the wrong name. Idempotent.
    @discardableResult
    public func putBlob(_ bytes: Data) async throws -> ContentHash {
        let hash = ContentHash.of(bytes)
        _ = try await client.send(
            "PUT", base + "/blobs/" + ProjectClient.escape(hash.description),
            accept: "application/json", contentType: "application/octet-stream", body: bytes)
        return hash
    }

    /// Download a blob and check it against its own name.
    ///
    /// Verification is unconditional: content addressing is only worth anything
    /// if the reader checks, and a caller who has to remember to do it
    /// separately eventually will not.
    public func getBlob(_ hash: ContentHash) async throws -> Data {
        let bytes = try await getBlobUnverified(hash)
        guard hash.matches(bytes) else {
            throw AuruError(
                code: .badRequest,
                message:
                    "blob \(hash) does not hash to its own name; the provider returned different bytes"
            )
        }
        return bytes
    }

    /// Download a blob without checking it. The caller owns verification.
    public func getBlobUnverified(_ hash: ContentHash) async throws -> Data {
        try await client.send(
            "GET", base + "/blobs/" + ProjectClient.escape(hash.description),
            accept: "application/octet-stream", contentType: nil, body: nil
        ).body
    }

    /// The ``ProjectInfo`` a commit points at.
    ///
    /// The call a dashboard should reach for: a few kilobytes rather than the
    /// snapshot's several megabytes. `nil` when the commit carries no summary.
    public func projectInfo(for commit: Commit) async throws -> ProjectInfo? {
        guard let metadata = commit.metadata else { return nil }
        return try ProjectInfo(parsing: try await getBlob(metadata))
    }
}

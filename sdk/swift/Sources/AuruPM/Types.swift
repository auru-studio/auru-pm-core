import Foundation

/// The wire protocol version this client speaks.
///
/// A client refuses to talk to a provider whose `/v1/health` reports anything
/// else: a mismatch means the shapes below are not the shapes it serves.
public let auruProtocolVersion = "auru-pm-v1"

/// What a provider implements.
///
/// Every flag defaults to false, which is what makes capabilities safe to add: a
/// provider written before one existed omits it and a client reads it as absent
/// rather than guessing.
public struct Capabilities: Sendable, Equatable {
    public var projectListing = false
    public var members = false
    public var permissions = false
    public var branches = false
    public var serverSideMerge = false
    public var compressedUploads = false
    public var historyRetention = false
    public var projectScopedBlobs = false
    public var authMethods: [String] = []

    init(json: JSON) {
        projectListing = json.bool("project_listing", default: false)
        members = json.bool("members", default: false)
        permissions = json.bool("permissions", default: false)
        branches = json.bool("branches", default: false)
        serverSideMerge = json.bool("server_side_merge", default: false)
        compressedUploads = json.bool("compressed_uploads", default: false)
        historyRetention = json.bool("history_retention", default: false)
        projectScopedBlobs = json.bool("project_scoped_blobs", default: false)
        authMethods = (json["auth_methods"]?.arrayValue ?? []).compactMap(\.stringValue)
    }

    init() {}
}

/// One registered public OAuth client.
public struct OAuthClient: Sendable, Equatable {
    /// Which kind of client this is.
    ///
    /// Not cosmetic: the two use different redirect rules and must not share a
    /// client id, because an identity provider's redirect allow-list is per
    /// client.
    public enum Kind: String, Sendable { case native, browser }

    public var kind: Kind
    public var clientID: String
    public var redirectURI: String
    public var flows: [String]

    public init(kind: Kind, clientID: String, redirectURI: String, flows: [String]) {
        self.kind = kind
        self.clientID = clientID
        self.redirectURI = redirectURI
        self.flows = flows
    }
}

/// A provider's public OAuth configuration.
///
/// Endpoint URLs are deliberately absent — a client discovers them from the
/// exact issuer. A provider that could name its own token endpoint could name
/// someone else's.
public struct OAuthConfiguration: Sendable, Equatable {
    public var issuer: String
    public var audience: String
    public var requiredScope: String
    public var clients: [OAuthClient]

    /// The registration of a given kind, if the provider published one.
    public func client(_ kind: OAuthClient.Kind) -> OAuthClient? {
        clients.first { $0.kind == kind }
    }

    init(json: JSON) {
        issuer = json["issuer"]?.stringValue ?? ""
        audience = json["audience"]?.stringValue ?? ""
        requiredScope = json["required_scope"]?.stringValue ?? "openid"

        clients = (json["clients"]?.arrayValue ?? []).map { entry in
            OAuthClient(
                kind: entry["kind"]?.stringValue == "browser" ? .browser : .native,
                clientID: entry["client_id"]?.stringValue ?? "",
                redirectURI: entry["redirect_uri"]?.stringValue ?? "",
                flows: (entry["flows"]?.arrayValue ?? []).compactMap(\.stringValue))
        }

        // The singular fields predate `clients` and describe the native client.
        // Reading them keeps a provider written before the list existed usable.
        if !clients.contains(where: { $0.kind == .native }),
            let clientID = json["client_id"]?.stringValue,
            let redirectURI = json["redirect_uri"]?.stringValue
        {
            clients.insert(
                OAuthClient(
                    kind: .native, clientID: clientID, redirectURI: redirectURI,
                    flows: (json["flows"]?.arrayValue ?? []).compactMap(\.stringValue)),
                at: 0)
        }
    }
}

/// What a provider says about itself, before any credential is presented.
public struct ProviderHealth: Sendable, Equatable {
    public var protocolVersion: String
    public var providerID: String?
    public var name: String?
    public var capabilities: Capabilities
    public var authentication: OAuthConfiguration?

    init(json: JSON) throws {
        protocolVersion = try json.string("protocol")
        providerID = json["provider_id"]?.stringValue
        name = json["name"]?.stringValue
        capabilities = json["capabilities"].map { Capabilities(json: $0) } ?? Capabilities()
        authentication = json["authentication"].map { OAuthConfiguration(json: $0) }
    }
}

/// Who a provider says you are, derived from the presented token.
///
/// The identity key is `(issuer, subject)`, never email.
public struct AuthenticatedIdentity: Sendable, Equatable {
    public var providerID: String
    public var userID: String
    public var displayName: String
    public var email: String?

    /// An author for commits written by this identity.
    ///
    /// A provider checks a commit's author against this and rejects a mismatch,
    /// so read it once and reuse it rather than composing one locally.
    public var asAuthor: AuthorIdentity {
        AuthorIdentity(
            displayName: displayName, providerUserID: userID, providerID: providerID, email: email)
    }

    init(json: JSON) throws {
        providerID = try json.string("provider_id")
        userID = try json.string("user_id")
        displayName = try json.string("display_name")
        email = json["email"]?.stringValue
    }
}

/// The human-facing metadata an account project list needs.
///
/// Registering one is also how a project handle comes into existence: until then
/// every other project-scoped endpoint answers `not_found`, blob upload
/// included.
public struct ProjectProfile: Sendable, Equatable {
    public var displayName: String
    public var format: String
    /// Comma-separated categories, e.g. "Drum & Bass, Jungle".
    public var genre: String?
    public var tags: [String]
    /// `/`-separated path beneath a library root, never absolute.
    public var relativePath: String?

    public init(
        displayName: String, format: String, genre: String? = nil, tags: [String] = [],
        relativePath: String? = nil
    ) {
        self.displayName = displayName
        self.format = format
        self.genre = genre
        self.tags = tags
        self.relativePath = relativePath
    }

    init(json: JSON) {
        displayName = json["display_name"]?.stringValue ?? ""
        format = json["format"]?.stringValue ?? ""
        genre = json["metadata"]?["genre"]?.stringValue
        tags = (json["metadata"]?["tags"]?.arrayValue ?? []).compactMap(\.stringValue)
        relativePath = json["location"]?["relative_path"]?.stringValue
    }

    var json: JSON {
        var metadata: [(String, JSON?)] = []
        if let genre { metadata.append(("genre", .string(genre))) }
        if !tags.isEmpty { metadata.append(("tags", .array(tags.map { .string($0) }))) }
        let metadataJSON = JSON.object(metadata)

        return .object([
            ("display_name", .string(displayName)),
            ("format", .string(format)),
            ("metadata", metadataJSON.objectMembers.isEmpty ? nil : metadataJSON),
            ("location", relativePath.map { JSON.object([("relative_path", .string($0))]) }),
        ])
    }
}

/// One project visible to the authenticated account.
public struct ProviderProject: Sendable, Equatable {
    public var handle: String
    public var head: ContentHash
    /// Absent for a project written before catalogues existed.
    public var profile: ProjectProfile?
    /// Unix epoch seconds of the HEAD commit.
    public var updatedAt: Int64

    init(json: JSON) throws {
        handle = try json.string("handle")
        head = try ContentHash(parsing: try json.string("head"))
        profile = json["profile"].map { ProjectProfile(json: $0) }
        updatedAt = try json.integer("updated_at")
    }
}

/// A history row. Omits the tree so listing does not force a fetch per row.
public struct CommitSummary: Sendable, Equatable {
    public var id: ContentHash
    public var parents: [ContentHash]
    public var author: AuthorIdentity
    public var timestamp: Int64
    public var message: String
    public var description: String

    init(json: JSON) throws {
        id = try ContentHash(parsing: try json.string("id"))
        parents = try (json["parents"]?.arrayValue ?? []).map {
            try ContentHash(parsing: $0.stringValue ?? "")
        }
        guard let authorJSON = json["author"] else {
            throw AuruError(code: .badRequest, message: "a history row has no author")
        }
        author = try AuthorIdentity(json: authorJSON)
        timestamp = try json.integer("timestamp")
        message = try json.string("message")
        description = json["description"]?.stringValue ?? ""
    }
}

/// The few-kilobyte summary of what a version is.
///
/// A real Live Set snapshot is around 7 MB. Every commit also stores this and
/// points at it with ``Commit/metadata``, so a project list can render without
/// ever fetching a snapshot.
///
/// The per-DAW body stays raw ``JSON``: each adapter grows fields as it learns to
/// read more of a format, and a fixed struct would have to be republished before
/// a caller could display a new one.
public struct ProjectInfo: Sendable, Equatable {
    public var schema: Int64
    public var format: String
    public var detail: JSON

    /// A text field of the per-DAW body, e.g. "title".
    public func text(_ name: String) -> String? { detail[name]?.stringValue }
    /// A numeric field of the per-DAW body, e.g. "tempo".
    public func number(_ name: String) -> Double? { detail[name]?.doubleValue }

    public init(parsing blob: Data) throws {
        try self.init(json: JSON.parse(blob))
    }

    init(json: JSON) throws {
        schema = try json.integer("schema")
        format = try json.string("format")
        detail = json["ableton"] ?? json["flstudio"] ?? json["dawproject"] ?? .null
    }
}

/// Destructive history policy.
///
/// There is deliberately no "keep everything" rule: keeping everything means not
/// calling the endpoint. Once a provider has removed a version, changing a
/// preference later cannot bring it back.
public enum RetentionRule: Sendable, Equatable {
    /// Keep the newest `count` versions. Providers always keep HEAD.
    case latest(count: Int32)
    /// Keep HEAD and the history prefix through the oldest commit at or after this time.
    case since(unixSeconds: Int64)

    var json: JSON {
        switch self {
        case .latest(let count):
            return .object([("policy", .string("latest")), ("count", .int(Int64(count)))])
        case .since(let seconds):
            return .object([("policy", .string("since")), ("timestamp", .int(seconds))])
        }
    }
}

public struct RetentionRequest: Sendable, Equatable {
    public var rule: RetentionRule
    /// In-flight client work a provider must preserve even when it falls outside
    /// the new boundary — a queued mirror push, a pre-merge stash.
    public var protectedCommits: [ContentHash]
    public var protectedBlobs: [ContentHash]

    public init(
        rule: RetentionRule, protectedCommits: [ContentHash] = [],
        protectedBlobs: [ContentHash] = []
    ) {
        self.rule = rule
        self.protectedCommits = protectedCommits
        self.protectedBlobs = protectedBlobs
    }

    var json: JSON {
        .object([
            ("rule", rule.json),
            (
                "protected_commits",
                protectedCommits.isEmpty
                    ? nil : .array(protectedCommits.map { .string($0.description) })
            ),
            (
                "protected_blobs",
                protectedBlobs.isEmpty ? nil : .array(protectedBlobs.map { .string($0.description) })
            ),
        ])
    }
}

public struct RetentionReport: Sendable, Equatable {
    public var versionsRemoved: Int64
    /// May be zero even when versions were removed: a provider is allowed to
    /// keep newly orphaned objects for a grace period.
    public var objectsRemoved: Int64
    public var bytesFreed: Int64

    init(json: JSON) throws {
        versionsRemoved = try json.integer("versions_removed")
        objectsRemoved = (try? json.integer("objects_removed")) ?? 0
        bytesFreed = (try? json.integer("bytes_freed")) ?? 0
    }
}

/// A page of history to request.
public struct HistoryRange: Sendable, Equatable {
    /// Zero for the provider's default.
    public var limit: Int32
    /// Return commits strictly older than this id.
    public var before: ContentHash?

    public init(limit: Int32 = 0, before: ContentHash? = nil) {
        self.limit = limit
        self.before = before
    }
}

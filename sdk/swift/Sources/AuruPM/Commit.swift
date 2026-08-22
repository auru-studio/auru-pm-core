import Foundation

/// Who made a version, captured at commit time.
///
/// Recorded inline rather than looked up so history renders without a live
/// provider connection, and so attribution survives someone leaving a workspace.
/// A provider checks these against the identity it derived from the bearer
/// token, so build one from ``AuruClient/me()`` rather than from local settings.
public struct AuthorIdentity: Sendable, Equatable {
    public var displayName: String
    public var providerUserID: String
    public var providerID: String
    public var email: String?

    public init(displayName: String, providerUserID: String, providerID: String, email: String? = nil) {
        self.displayName = displayName
        self.providerUserID = providerUserID
        self.providerID = providerID
        self.email = email
    }

    init(json: JSON) throws {
        self.init(
            displayName: try json.string("display_name"),
            providerUserID: try json.string("provider_user_id"),
            providerID: try json.string("provider_id"),
            email: json["email"]?.stringValue)
    }

    var json: JSON {
        .object([
            ("display_name", .string(displayName)),
            ("provider_user_id", .string(providerUserID)),
            ("provider_id", .string(providerID)),
            ("email", email.map { JSON.string($0) }),
        ])
    }
}

/// What a commit points at.
public struct TreeRef: Sendable, Equatable {
    /// Blob holding the canonical project JSON for this version.
    public var snapshot: ContentHash
    /// Blob listing the `(path, hash)` pairs the project depends on. Samples
    /// download lazily, so this is the cheap "what do I need before playback"
    /// probe.
    public var samples: ContentHash

    public init(snapshot: ContentHash, samples: ContentHash) {
        self.snapshot = snapshot
        self.samples = samples
    }

    init(json: JSON) throws {
        self.init(
            snapshot: try ContentHash(parsing: try json.string("snapshot")),
            samples: try ContentHash(parsing: try json.string("samples")))
    }

    var json: JSON {
        .object([
            ("snapshot", .string(snapshot.description)),
            ("samples", .string(samples.description)),
        ])
    }
}

/// One version in a project's history.
///
/// `parents.count` is the shape: 0 root, 1 normal, 2 merge.
///
/// The id is the BLAKE3 of the RFC 8785 canonicalization of every other field.
/// Providers recompute it and treat a mismatch as auth-equivalent — a client
/// writing what it did not compute — so no initializer accepts one.
/// ``init(parents:tree:author:timestamp:message:description:auruVersion:formatVersion:metadata:)``
/// derives it; ``init(json:)`` keeps the one a provider sent, and ``verifyID()``
/// checks it.
public struct Commit: Sendable, Equatable {

    public let id: ContentHash
    public let parents: [ContentHash]
    public let tree: TreeRef
    public let author: AuthorIdentity
    /// Unix epoch seconds.
    public let timestamp: Int64
    public let message: String
    public let description: String
    public let auruVersion: String
    /// Snapshot schema version at commit time, so a reader knows whether it
    /// needs migration before restore.
    public let formatVersion: Int64
    /// Blob holding this commit's ``ProjectInfo`` — a few kilobytes of tempo,
    /// key, tracks and plugins.
    ///
    /// This is what lets a project list render without fetching the snapshot,
    /// which for a real Live Set is around 7 MB. Absent on older commits and on
    /// formats the writer could not summarize; a reader that finds it missing
    /// falls back to the snapshot.
    public let metadata: ContentHash?

    /// Assemble a commit and derive its id.
    ///
    /// - Throws: when there are more than two parents, or a number falls outside
    ///   what RFC 8785 can represent.
    public init(
        parents: [ContentHash] = [],
        tree: TreeRef,
        author: AuthorIdentity,
        timestamp: Int64,
        message: String,
        description: String = "",
        auruVersion: String,
        formatVersion: Int64,
        metadata: ContentHash? = nil
    ) throws {
        guard parents.count <= 2 else {
            throw AuruError(
                code: .badRequest,
                message:
                    "a commit has at most two parents (0 root, 1 normal, 2 merge), got \(parents.count)")
        }

        self.parents = parents
        self.tree = tree
        self.author = author
        self.timestamp = timestamp
        self.message = message
        self.description = description
        self.auruVersion = auruVersion
        self.formatVersion = formatVersion
        self.metadata = metadata

        let canonical = try Commit.contentJSON(
            parents: parents, tree: tree, author: author, timestamp: timestamp, message: message,
            description: description, auruVersion: auruVersion, formatVersion: formatVersion,
            metadata: metadata
        ).canonicalJSON()
        self.id = ContentHash.of(canonical)
    }

    /// Read a commit as a provider sent it, keeping its id.
    public init(json: JSON) throws {
        guard let parentsJSON = json["parents"] else {
            throw AuruError(code: .badRequest, message: "missing required member \"parents\"")
        }
        var parents: [ContentHash] = []
        for parent in parentsJSON.arrayValue {
            guard let text = parent.stringValue else {
                throw AuruError(code: .badRequest, message: "a parent is not a string")
            }
            parents.append(try ContentHash(parsing: text))
        }

        guard let treeJSON = json["tree"] else {
            throw AuruError(code: .badRequest, message: "missing required member \"tree\"")
        }
        guard let authorJSON = json["author"] else {
            throw AuruError(code: .badRequest, message: "missing required member \"author\"")
        }

        self.id = try ContentHash(parsing: try json.string("id"))
        self.parents = parents
        self.tree = try TreeRef(json: treeJSON)
        self.author = try AuthorIdentity(json: authorJSON)
        self.timestamp = try json.integer("timestamp")
        self.message = try json.string("message")
        self.description = json["description"]?.stringValue ?? ""
        self.auruVersion = try json.string("auru_version")
        self.formatVersion = try json.integer("format_version")
        self.metadata = try json["metadata"]?.stringValue.map { try ContentHash(parsing: $0) }
    }

    /// The exact bytes this commit's id is the BLAKE3 of.
    ///
    /// Worth having when a provider rejects a commit: comparing these against
    /// `spec/vectors/commit-encoding.json` says immediately which side is wrong.
    public func canonicalEncoding() throws -> String {
        try contentJSON.canonicalJSON()
    }

    /// Whether ``id`` matches this commit's content.
    public func verifyID() -> Bool {
        guard let canonical = try? canonicalEncoding() else { return false }
        return ContentHash.of(canonical) == id
    }

    /// The wire form, id included.
    public var json: JSON {
        var members: [JSONMember] = [JSONMember("id", .string(id.description))]
        members.append(contentsOf: contentJSON.objectMembers)
        return .object(members)
    }

    private var contentJSON: JSON {
        Commit.contentJSON(
            parents: parents, tree: tree, author: author, timestamp: timestamp, message: message,
            description: description, auruVersion: auruVersion, formatVersion: formatVersion,
            metadata: metadata)
    }

    /// Everything except the id — identity is a function of content, not itself.
    private static func contentJSON(
        parents: [ContentHash], tree: TreeRef, author: AuthorIdentity, timestamp: Int64,
        message: String, description: String, auruVersion: String, formatVersion: Int64,
        metadata: ContentHash?
    ) -> JSON {
        .object([
            ("parents", .array(parents.map { .string($0.description) })),
            ("tree", tree.json),
            ("author", author.json),
            ("timestamp", .int(timestamp)),
            ("message", .string(message)),
            ("description", .string(description)),
            ("auru_version", .string(auruVersion)),
            ("format_version", .int(formatVersion)),
            ("metadata", metadata.map { JSON.string($0.description) }),
        ])
    }
}

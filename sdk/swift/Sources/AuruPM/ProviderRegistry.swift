import Foundation

/// A published list of providers, for the "which backup do you use?" step of
/// first run.
///
/// A registry is a convenience, never an authority. It says a provider exists
/// and where to find it; everything that matters — the protocol version, the
/// capabilities, which authentication methods actually work — comes from that
/// provider's own `GET /v1/health` once connected. An entry here is a suggestion
/// the user is free to ignore, and any endpoint they type by hand is equally
/// valid.
///
/// **A registry that cannot be fetched produces an error, never a list.**
/// ``fetch(from:transport:)`` throws rather than returning something partial or
/// invented. Filling the gap with a plausible default would put entries in front
/// of someone that they cannot connect to, which is strictly worse than showing
/// nothing and offering them the address field.
public struct ProviderRegistry: Sendable, Equatable {

    /// The Auru-curated registry.
    public static let auruRegistryURL = "https://pm.auru.studio/providers.json"

    public var providers: [Entry]

    public init(providers: [Entry]) {
        self.providers = providers
    }

    /// One provider, as the registry describes it.
    public struct Entry: Sendable, Equatable {
        public var id: String
        public var name: String
        public var endpoint: String
        public var description: String
        public var detail: String
        /// What signing in will involve — worth showing before the user commits,
        /// because "you will be sent to your browser" and "go and create a token
        /// somewhere" are very different amounts of work. Empty when the entry
        /// claims nothing.
        public var authMethods: [String]
        public var recommended: Bool
        public var iconURL: String?

        public init(
            id: String, name: String, endpoint: String, description: String = "",
            detail: String = "", authMethods: [String] = [], recommended: Bool = false,
            iconURL: String? = nil
        ) {
            self.id = id
            self.name = name
            self.endpoint = endpoint
            self.description = description
            self.detail = detail
            self.authMethods = authMethods
            self.recommended = recommended
            self.iconURL = iconURL
        }
    }

    /// Fetch and parse a registry.
    ///
    /// - Throws: if it cannot be reached, does not answer 200, or is not a registry.
    public static func fetch(from url: String, transport: any Transport) async throws
        -> ProviderRegistry
    {
        let response = try await transport.send(
            HTTPRequest(
                method: "GET", url: url,
                headers: [(name: "accept", value: "application/json")]))

        guard response.status == 200 else {
            throw AuruError(code: .internalError, message: "\(url) answered HTTP \(response.status)")
        }
        return try parse(response.body)
    }

    /// Parse a registry document.
    ///
    /// A malformed entry fails the whole document rather than being skipped. A
    /// registry is hand-maintained, and silently dropping the one provider
    /// somebody just added to it is a bug report nobody can reproduce.
    public static func parse(_ document: Data) throws -> ProviderRegistry {
        let json: JSON
        do {
            json = try JSON.parse(document)
        } catch {
            throw AuruError(
                code: .internalError, message: "the provider registry is not JSON: \(error)")
        }

        func required(_ entry: JSON, _ field: String) throws -> String {
            guard let value = entry[field]?.stringValue, !value.isEmpty else {
                throw AuruError(
                    code: .internalError, message: "a provider registry entry has no \(field)")
            }
            return value
        }

        let entries = try (json["providers"]?.arrayValue ?? []).map { entry in
            Entry(
                id: try required(entry, "id"),
                name: try required(entry, "name"),
                endpoint: try required(entry, "endpoint"),
                description: entry["description"]?.stringValue ?? "",
                detail: entry["detail"]?.stringValue ?? "",
                authMethods: (entry["auth_methods"]?.arrayValue ?? []).compactMap(\.stringValue),
                recommended: entry["recommended"]?.boolValue ?? false,
                iconURL: entry["icon_url"]?.stringValue)
        }
        return ProviderRegistry(providers: entries)
    }
}

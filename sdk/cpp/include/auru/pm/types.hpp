#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <vector>

#include "auru/pm/commit.hpp"
#include "auru/pm/content_hash.hpp"
#include "auru/pm/json.hpp"
#include "auru/pm/result.hpp"

namespace auru::pm {

/// The wire protocol version this client speaks.
///
/// A client refuses to talk to a provider whose `/v1/health` reports anything
/// else: a mismatch means the shapes below are not the shapes it serves.
inline constexpr const char* kProtocolVersion = "auru-pm-v1";

/// What a provider implements.
///
/// Every flag defaults to false, which is what makes capabilities safe to add:
/// a provider written before one existed omits it and a client reads it as
/// absent rather than guessing.
struct Capabilities {
    bool project_listing = false;
    bool members = false;
    bool permissions = false;
    bool branches = false;
    bool server_side_merge = false;
    bool compressed_uploads = false;
    bool history_retention = false;
    bool project_scoped_blobs = false;
    std::vector<std::string> auth_methods;

    static Capabilities from_json(const Json& json);
};

/// One registered public OAuth client.
struct OAuthClient {
    /// Which kind of client this is.
    ///
    /// Not cosmetic: each kind uses different redirect rules and they must not
    /// share a client id, because an identity provider's redirect allow-list
    /// is per client. `Unknown` is a kind this build does not know — kept
    /// rather than mislabelled, so a registration for a client newer than this
    /// SDK is never mistaken for one of the others.
    enum class Kind { Native, Browser, Mobile, Unknown };

    Kind kind = Kind::Native;
    std::string client_id;
    std::string redirect_uri;
    std::vector<std::string> flows;
};

/// A provider's public OAuth configuration.
///
/// Endpoint URLs are deliberately absent — a client discovers them from the
/// exact issuer. A provider that could name its own token endpoint could name
/// someone else's.
struct OAuthConfiguration {
    std::string issuer;
    std::string audience;
    std::string required_scope = "openid";
    std::vector<OAuthClient> clients;

    /// The registration of a given kind, if the provider published one.
    std::optional<OAuthClient> client(OAuthClient::Kind kind) const;

    static OAuthConfiguration from_json(const Json& json);
};

/// What a provider says about itself, before any credential is presented.
struct ProviderHealth {
    std::string protocol;
    std::optional<std::string> provider_id;
    std::optional<std::string> name;
    Capabilities capabilities;
    std::optional<OAuthConfiguration> authentication;

    static Result<ProviderHealth> from_json(const Json& json);
};

/// Who a provider says you are, derived from the presented token.
///
/// The identity key is `(issuer, subject)`, never email.
struct AuthenticatedIdentity {
    std::string provider_id;
    std::string user_id;
    std::string display_name;
    std::optional<std::string> email;

    /// An author for commits written by this identity.
    ///
    /// A provider checks a commit's author against this and rejects a mismatch,
    /// so read it once and reuse it rather than composing one locally.
    AuthorIdentity as_author() const;

    static Result<AuthenticatedIdentity> from_json(const Json& json);
};

/// The human-facing metadata an account project list needs.
///
/// Registering one is also how a project handle comes into existence: until
/// then every other project-scoped endpoint answers `not_found`, blob upload
/// included.
struct ProjectProfile {
    std::string display_name;
    std::string format;
    /// Comma-separated categories, e.g. "Drum & Bass, Jungle".
    std::optional<std::string> genre;
    std::vector<std::string> tags;
    /// `/`-separated path beneath a library root, never absolute.
    std::optional<std::string> relative_path;

    Json to_json() const;
    static ProjectProfile from_json(const Json& json);
};

/// One project visible to the authenticated account.
struct ProviderProject {
    std::string handle;
    ContentHash head = ContentHash::of("");
    /// Absent for a project written before catalogues existed.
    std::optional<ProjectProfile> profile;
    /// Unix epoch seconds of the HEAD commit.
    std::int64_t updated_at = 0;

    static Result<ProviderProject> from_json(const Json& json);
};

/// A history row. Omits the tree so listing does not force a fetch per row.
struct CommitSummary {
    ContentHash id = ContentHash::of("");
    std::vector<ContentHash> parents;
    AuthorIdentity author;
    std::int64_t timestamp = 0;
    std::string message;
    std::string description;

    static Result<CommitSummary> from_json(const Json& json);
};

/// The few-kilobyte summary of what a version is.
///
/// A real Live Set snapshot is around 7 MB. Every commit also stores this and
/// points at it with `Commit::metadata`, so a project list can render without
/// ever fetching a snapshot.
///
/// The per-DAW body stays raw `Json`: each adapter grows fields as it learns to
/// read more of a format, and a fixed struct would have to be republished
/// before a caller could display a new one.
struct ProjectInfo {
    std::int64_t schema = 0;
    std::string format;
    Json detail;

    /// A text field of the per-DAW body, e.g. "title".
    std::optional<std::string> text(const std::string& name) const;
    /// A numeric field of the per-DAW body, e.g. "tempo".
    std::optional<double> number(const std::string& name) const;

    static Result<ProjectInfo> parse(const std::vector<std::uint8_t>& blob);
    static Result<ProjectInfo> from_json(const Json& json);
};

/// Destructive history policy.
///
/// There is deliberately no "keep everything" rule: keeping everything means
/// not calling the endpoint. Once a provider has removed a version, changing a
/// preference later cannot bring it back.
struct RetentionRule {
    enum class Policy { Latest, Since };

    Policy policy = Policy::Latest;
    /// Versions to keep, for Latest. Providers always keep HEAD.
    std::int32_t count = 0;
    /// Unix seconds, for Since.
    std::int64_t timestamp = 0;

    static RetentionRule latest(std::int32_t count);
    static RetentionRule since(std::int64_t unix_seconds);
};

struct RetentionRequest {
    RetentionRule rule;
    /// In-flight client work a provider must preserve even when it falls
    /// outside the new boundary — a queued mirror push, a pre-merge stash.
    std::vector<ContentHash> protected_commits;
    std::vector<ContentHash> protected_blobs;

    Json to_json() const;
};

struct RetentionReport {
    std::int64_t versions_removed = 0;
    /// May be zero even when versions were removed: a provider is allowed to
    /// keep newly orphaned objects for a grace period.
    std::int64_t objects_removed = 0;
    std::int64_t bytes_freed = 0;

    static Result<RetentionReport> from_json(const Json& json);
};

/// A page of history to request.
struct HistoryRange {
    /// Zero for the provider's default.
    std::int32_t limit = 0;
    /// Return commits strictly older than this id.
    std::optional<ContentHash> before;
};

}  // namespace auru::pm

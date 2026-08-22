#pragma once

#include <memory>
#include <optional>
#include <string>
#include <vector>

#include "auru/pm/commit.hpp"
#include "auru/pm/result.hpp"
#include "auru/pm/transport.hpp"
#include "auru/pm/types.hpp"

namespace auru::pm {

class ProjectClient;

/// A connected `auru-pm-v1` provider.
///
/// The endpoint is always supplied by the caller. There is no default host here
/// and no registry lookup: anyone can run a provider, and which one to trust is
/// the user's decision rather than this library's.
///
/// ```cpp
/// auto client = AuruClient::connect({"https://pm.example.com", transport, token});
/// if (!client) { return client.error(); }
///
/// auto project = client.value().project("user/night-drive");
/// for (const auto& version : project.value().history({20}).value()) {
///     std::cout << version.timestamp << "  " << version.message << "\n";
/// }
/// ```
///
/// Copying an `AuruClient` shares its state, including the bearer token, so a
/// refresh on one copy is visible to the rest.
class AuruClient {
public:
    struct Options {
        /// Provider base URL, e.g. `https://pm.example.com`.
        std::string endpoint;
        /// How to reach it. Required — the library has no built-in HTTP stack.
        std::shared_ptr<Transport> transport;
        /// Bearer token, held in memory and written nowhere else.
        std::optional<std::string> access_token;
    };

    /// Read `/v1/health` and return a client bound to that provider.
    ///
    /// Health is read first because the protocol version, the auth methods and
    /// the capability set all have to be known before the first real call — one
    /// round trip up front is cheaper than discovering an unsupported endpoint
    /// halfway through a push.
    static Result<AuruClient> connect(Options options);

    const ProviderHealth& health() const;
    const Capabilities& capabilities() const;
    const std::string& endpoint() const;

    /// Replace the bearer token, after a refresh for instance.
    void set_access_token(std::optional<std::string> token);
    /// Whether a token is held. Never exposes the token itself.
    bool authenticated() const;

    /// The identity the provider derived from the current token.
    Result<AuthenticatedIdentity> me() const;

    /// Projects visible to this account, newest first.
    Result<std::vector<ProviderProject>> list_projects() const;

    /// Scope subsequent calls to one project handle.
    Result<ProjectClient> project(std::string handle) const;

    struct State;

private:
    explicit AuruClient(std::shared_ptr<State> state) : state_(std::move(state)) {}

    friend class ProjectClient;

    std::shared_ptr<State> state_;
};

/// Calls scoped to one project handle.
class ProjectClient {
public:
    const std::string& handle() const noexcept { return handle_; }

    /// Register this project's human-facing metadata.
    ///
    /// Also how the handle comes into existence: until a profile is registered
    /// every other project-scoped call answers `not_found`, blob upload
    /// included. Publish one before uploading anything.
    Result<void> put_profile(const ProjectProfile& profile) const;

    /// The current HEAD, empty on a project with no commits.
    Result<std::optional<ContentHash>> head() const;

    /// Compare-and-swap HEAD from `from` to `to`.
    ///
    /// Pass an empty `from` for the initial publish. On failure the error is
    /// `ErrorCode::HeadConflict` with `current_head` set to the provider's
    /// actual HEAD, so a caller can rebase without asking again.
    Result<void> advance_head(const std::optional<ContentHash>& from, const ContentHash& to) const;

    /// Store a commit.
    ///
    /// The provider recomputes the id from the canonical encoding and rejects a
    /// mismatch — writing a commit you did not compute is an auth-equivalent
    /// failure, not a formatting slip. A `Commit` always carries a derived id,
    /// so this cannot be got wrong by construction.
    ///
    /// Idempotent: re-posting an existing id succeeds.
    Result<ContentHash> put_commit(const Commit& commit) const;

    Result<Commit> get_commit(const ContentHash& id) const;

    /// History newest first.
    Result<std::vector<CommitSummary>> history(const HistoryRange& range = {}) const;

    /// Permanently move the oldest visible-history boundary. Irreversible.
    Result<RetentionReport> prune_history(const RetentionRequest& request) const;

    /// Which of `hashes` the provider already holds, parallel-indexed.
    Result<std::vector<bool>> has_blobs(const std::vector<ContentHash>& hashes) const;

    /// Upload a blob under its own hash.
    ///
    /// The hash is derived from the bytes rather than accepted from the caller,
    /// so an upload cannot be filed under the wrong name.
    Result<ContentHash> put_blob(const std::vector<std::uint8_t>& bytes) const;

    /// Download a blob and check it against its own name.
    ///
    /// Verification is unconditional: content addressing is only worth anything
    /// if the reader checks, and a caller who has to remember to do it
    /// separately eventually will not.
    Result<std::vector<std::uint8_t>> get_blob(const ContentHash& hash) const;

    /// Download a blob without checking it. The caller owns verification.
    Result<std::vector<std::uint8_t>> get_blob_unverified(const ContentHash& hash) const;

    /// The `ProjectInfo` a commit points at.
    ///
    /// The call a dashboard should reach for: a few kilobytes rather than the
    /// snapshot's several megabytes. Empty when the commit carries no summary.
    Result<std::optional<ProjectInfo>> project_info(const Commit& commit) const;

private:
    friend class AuruClient;

    ProjectClient(std::shared_ptr<AuruClient::State> state, std::string handle);

    std::shared_ptr<AuruClient::State> state_;
    std::string handle_;
    std::string base_;
};

}  // namespace auru::pm

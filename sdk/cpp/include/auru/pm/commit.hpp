#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <vector>

#include "auru/pm/content_hash.hpp"
#include "auru/pm/json.hpp"
#include "auru/pm/result.hpp"

namespace auru::pm {

/// Who made a version, captured at commit time.
///
/// Recorded inline rather than looked up so history renders without a live
/// provider connection, and so attribution survives someone leaving a
/// workspace. A provider checks these against the identity it derived from the
/// bearer token, so build one from `AuruClient::me()` rather than from local
/// settings.
struct AuthorIdentity {
    std::string display_name;
    std::string provider_user_id;
    std::string provider_id;
    std::optional<std::string> email;

    Json to_json() const;
    static Result<AuthorIdentity> from_json(const Json& json);
};

/// What a commit points at.
struct TreeRef {
    /// Blob holding the canonical project JSON for this version.
    ContentHash snapshot;
    /// Blob listing the `(path, hash)` pairs the project depends on. Samples
    /// download lazily, so this is the cheap "what do I need before playback"
    /// probe.
    ContentHash samples;

    Json to_json() const;
    static Result<TreeRef> from_json(const Json& json);
};

/// One version in a project's history.
///
/// `parents.size()` is the shape: 0 root, 1 normal, 2 merge.
///
/// The id is the BLAKE3 of the RFC 8785 canonicalization of every other field.
/// Providers recompute it and treat a mismatch as auth-equivalent — a client
/// writing what it did not compute — so there is no way to construct one with
/// an id of your choosing. `Commit::Builder` derives it; `from_json` keeps the
/// one a provider sent, and `verify_id` checks it.
class Commit {
public:
    class Builder;

    const ContentHash& id() const noexcept { return id_; }
    const std::vector<ContentHash>& parents() const noexcept { return parents_; }
    const TreeRef& tree() const noexcept { return tree_; }
    const AuthorIdentity& author() const noexcept { return author_; }
    /// Unix epoch seconds.
    std::int64_t timestamp() const noexcept { return timestamp_; }
    const std::string& message() const noexcept { return message_; }
    const std::string& description() const noexcept { return description_; }
    const std::string& auru_version() const noexcept { return auru_version_; }
    /// Snapshot schema version at commit time, so a reader knows whether it
    /// needs migration before restore.
    std::int64_t format_version() const noexcept { return format_version_; }

    /// Blob holding this commit's ProjectInfo — a few kilobytes of tempo, key,
    /// tracks and plugins.
    ///
    /// This is what lets a project list render without fetching the snapshot,
    /// which for a real Live Set is around 7 MB. Absent on older commits and on
    /// formats the writer could not summarize; a reader that finds it missing
    /// falls back to the snapshot.
    const std::optional<ContentHash>& metadata() const noexcept { return metadata_; }

    /// The exact bytes this commit's id is the BLAKE3 of.
    ///
    /// Worth having when a provider rejects a commit: comparing these against
    /// `spec/vectors/commit-encoding.json` says immediately which side is wrong.
    Result<std::string> canonical_encoding() const;

    /// Whether `id()` matches this commit's content.
    bool verify_id() const;

    Json to_json() const;

    /// Read a commit as a provider sent it, keeping its id.
    static Result<Commit> from_json(const Json& json);

private:
    friend class Builder;

    Json to_json_without_id() const;

    ContentHash id_ = ContentHash::of("");
    std::vector<ContentHash> parents_;
    TreeRef tree_{ContentHash::of(""), ContentHash::of("")};
    AuthorIdentity author_;
    std::int64_t timestamp_ = 0;
    std::string message_;
    std::string description_;
    std::string auru_version_;
    std::int64_t format_version_ = 0;
    std::optional<ContentHash> metadata_;
};

/// Assembles a commit and derives its id.
class Commit::Builder {
public:
    Builder& parents(std::vector<ContentHash> value);
    Builder& parent(ContentHash value);
    Builder& tree(TreeRef value);
    Builder& author(AuthorIdentity value);
    /// Unix epoch seconds.
    Builder& timestamp(std::int64_t value);
    Builder& message(std::string value);
    Builder& description(std::string value);
    Builder& auru_version(std::string value);
    Builder& format_version(std::int64_t value);
    Builder& metadata(ContentHash value);

    /// Derive the id and produce the commit.
    ///
    /// Fails when a required field is missing, when there are more than two
    /// parents, or when a number falls outside what RFC 8785 can represent.
    Result<Commit> build() const;

private:
    std::vector<ContentHash> parents_;
    std::optional<TreeRef> tree_;
    std::optional<AuthorIdentity> author_;
    std::optional<std::int64_t> timestamp_;
    std::optional<std::string> message_;
    std::string description_;
    std::optional<std::string> auru_version_;
    std::optional<std::int64_t> format_version_;
    std::optional<ContentHash> metadata_;
};

}  // namespace auru::pm

#include "auru/pm/commit.hpp"

#include <utility>

namespace auru::pm {

// ── AuthorIdentity ───────────────────────────────────────────────────────────

Json AuthorIdentity::to_json() const {
    Json json = Json::object({});
    json.set("display_name", Json::string(display_name));
    json.set("provider_user_id", Json::string(provider_user_id));
    json.set("provider_id", Json::string(provider_id));
    if (email) {
        json.set("email", Json::string(*email));
    }
    return json;
}

Result<AuthorIdentity> AuthorIdentity::from_json(const Json& json) {
    auto display_name = json.string_at("display_name");
    if (!display_name) {
        return Result<AuthorIdentity>::fail(display_name.error());
    }
    auto provider_user_id = json.string_at("provider_user_id");
    if (!provider_user_id) {
        return Result<AuthorIdentity>::fail(provider_user_id.error());
    }
    auto provider_id = json.string_at("provider_id");
    if (!provider_id) {
        return Result<AuthorIdentity>::fail(provider_id.error());
    }

    AuthorIdentity author;
    author.display_name = std::move(display_name).value();
    author.provider_user_id = std::move(provider_user_id).value();
    author.provider_id = std::move(provider_id).value();
    author.email = json.optional_string("email");
    return Result<AuthorIdentity>::ok(std::move(author));
}

// ── TreeRef ──────────────────────────────────────────────────────────────────

Json TreeRef::to_json() const {
    Json json = Json::object({});
    json.set("snapshot", Json::string(snapshot.to_string()));
    json.set("samples", Json::string(samples.to_string()));
    return json;
}

Result<TreeRef> TreeRef::from_json(const Json& json) {
    auto snapshot_text = json.string_at("snapshot");
    if (!snapshot_text) {
        return Result<TreeRef>::fail(snapshot_text.error());
    }
    auto samples_text = json.string_at("samples");
    if (!samples_text) {
        return Result<TreeRef>::fail(samples_text.error());
    }
    auto snapshot = ContentHash::parse(snapshot_text.value());
    if (!snapshot) {
        return Result<TreeRef>::fail(snapshot.error());
    }
    auto samples = ContentHash::parse(samples_text.value());
    if (!samples) {
        return Result<TreeRef>::fail(samples.error());
    }
    return Result<TreeRef>::ok(TreeRef{snapshot.value(), samples.value()});
}

// ── Commit ───────────────────────────────────────────────────────────────────

Json Commit::to_json_without_id() const {
    std::vector<Json> parent_json;
    parent_json.reserve(parents_.size());
    for (const ContentHash& parent : parents_) {
        parent_json.push_back(Json::string(parent.to_string()));
    }

    Json json = Json::object({});
    json.set("parents", Json::array(std::move(parent_json)));
    json.set("tree", tree_.to_json());
    json.set("author", author_.to_json());
    json.set("timestamp", Json::integer(timestamp_));
    json.set("message", Json::string(message_));
    json.set("description", Json::string(description_));
    json.set("auru_version", Json::string(auru_version_));
    json.set("format_version", Json::integer(format_version_));
    if (metadata_) {
        json.set("metadata", Json::string(metadata_->to_string()));
    }
    return json;
}

Json Commit::to_json() const {
    // The temporary is named on purpose. `members()` returns a reference into
    // it, and a range-for over `to_json_without_id().members()` would iterate a
    // container whose owner has already been destroyed — C++ does not extend
    // the lifetime through the call, and did not until C++23.
    const Json without_id = to_json_without_id();

    Json json = Json::object({});
    json.set("id", Json::string(id_.to_string()));
    for (const auto& member : without_id.members()) {
        json.set(member.first, member.second);
    }
    return json;
}

Result<std::string> Commit::canonical_encoding() const {
    return to_json_without_id().to_canonical_json();
}

bool Commit::verify_id() const {
    auto canonical = canonical_encoding();
    if (!canonical) {
        return false;
    }
    const std::string& bytes = canonical.value();
    return ContentHash::of(bytes.data(), bytes.size()) == id_;
}

Result<Commit> Commit::from_json(const Json& json) {
    auto id_text = json.string_at("id");
    if (!id_text) {
        return Result<Commit>::fail(id_text.error());
    }
    auto id = ContentHash::parse(id_text.value());
    if (!id) {
        return Result<Commit>::fail(id.error());
    }

    const Json* parents_json = json.find("parents");
    if (parents_json == nullptr) {
        return make_error<Commit>(ErrorCode::BadRequest, "missing required member \"parents\"");
    }
    std::vector<ContentHash> parents;
    for (const Json& parent : parents_json->elements()) {
        auto text = parent.as_text();
        if (!text) {
            return make_error<Commit>(ErrorCode::BadRequest, "a parent is not a string");
        }
        auto hash = ContentHash::parse(*text);
        if (!hash) {
            return Result<Commit>::fail(hash.error());
        }
        parents.push_back(hash.value());
    }

    const Json* tree_json = json.find("tree");
    if (tree_json == nullptr) {
        return make_error<Commit>(ErrorCode::BadRequest, "missing required member \"tree\"");
    }
    auto tree = TreeRef::from_json(*tree_json);
    if (!tree) {
        return Result<Commit>::fail(tree.error());
    }

    const Json* author_json = json.find("author");
    if (author_json == nullptr) {
        return make_error<Commit>(ErrorCode::BadRequest, "missing required member \"author\"");
    }
    auto author = AuthorIdentity::from_json(*author_json);
    if (!author) {
        return Result<Commit>::fail(author.error());
    }

    auto timestamp = json.integer_at("timestamp");
    if (!timestamp) {
        return Result<Commit>::fail(timestamp.error());
    }
    auto message = json.string_at("message");
    if (!message) {
        return Result<Commit>::fail(message.error());
    }
    auto auru_version = json.string_at("auru_version");
    if (!auru_version) {
        return Result<Commit>::fail(auru_version.error());
    }
    auto format_version = json.integer_at("format_version");
    if (!format_version) {
        return Result<Commit>::fail(format_version.error());
    }

    Commit commit;
    commit.id_ = id.value();
    commit.parents_ = std::move(parents);
    commit.tree_ = tree.value();
    commit.author_ = std::move(author).value();
    commit.timestamp_ = timestamp.value();
    commit.message_ = std::move(message).value();
    commit.description_ = json.optional_string("description").value_or("");
    commit.auru_version_ = std::move(auru_version).value();
    commit.format_version_ = format_version.value();

    if (auto metadata_text = json.optional_string("metadata")) {
        auto metadata = ContentHash::parse(*metadata_text);
        if (!metadata) {
            return Result<Commit>::fail(metadata.error());
        }
        commit.metadata_ = metadata.value();
    }
    return Result<Commit>::ok(std::move(commit));
}

// ── Builder ──────────────────────────────────────────────────────────────────

Commit::Builder& Commit::Builder::parents(std::vector<ContentHash> value) {
    parents_ = std::move(value);
    return *this;
}

Commit::Builder& Commit::Builder::parent(ContentHash value) {
    parents_ = {value};
    return *this;
}

Commit::Builder& Commit::Builder::tree(TreeRef value) {
    tree_ = value;
    return *this;
}

Commit::Builder& Commit::Builder::author(AuthorIdentity value) {
    author_ = std::move(value);
    return *this;
}

Commit::Builder& Commit::Builder::timestamp(std::int64_t value) {
    timestamp_ = value;
    return *this;
}

Commit::Builder& Commit::Builder::message(std::string value) {
    message_ = std::move(value);
    return *this;
}

Commit::Builder& Commit::Builder::description(std::string value) {
    description_ = std::move(value);
    return *this;
}

Commit::Builder& Commit::Builder::auru_version(std::string value) {
    auru_version_ = std::move(value);
    return *this;
}

Commit::Builder& Commit::Builder::format_version(std::int64_t value) {
    format_version_ = value;
    return *this;
}

Commit::Builder& Commit::Builder::metadata(ContentHash value) {
    metadata_ = value;
    return *this;
}

Result<Commit> Commit::Builder::build() const {
    if (!tree_) {
        return make_error<Commit>(ErrorCode::BadRequest, "a commit needs a tree");
    }
    if (!author_) {
        return make_error<Commit>(ErrorCode::BadRequest, "a commit needs an author");
    }
    if (!timestamp_) {
        return make_error<Commit>(ErrorCode::BadRequest, "a commit needs a timestamp");
    }
    if (!message_) {
        return make_error<Commit>(ErrorCode::BadRequest, "a commit needs a message");
    }
    if (!auru_version_) {
        return make_error<Commit>(ErrorCode::BadRequest, "a commit needs an auru_version");
    }
    if (!format_version_) {
        return make_error<Commit>(ErrorCode::BadRequest, "a commit needs a format_version");
    }
    if (parents_.size() > 2) {
        return make_error<Commit>(
            ErrorCode::BadRequest,
            "a commit has at most two parents (0 root, 1 normal, 2 merge), got "
                + std::to_string(parents_.size()));
    }

    Commit commit;
    commit.parents_ = parents_;
    commit.tree_ = *tree_;
    commit.author_ = *author_;
    commit.timestamp_ = *timestamp_;
    commit.message_ = *message_;
    commit.description_ = description_;
    commit.auru_version_ = *auru_version_;
    commit.format_version_ = *format_version_;
    commit.metadata_ = metadata_;

    auto canonical = commit.canonical_encoding();
    if (!canonical) {
        return Result<Commit>::fail(canonical.error());
    }
    const std::string& bytes = canonical.value();
    commit.id_ = ContentHash::of(bytes.data(), bytes.size());
    return Result<Commit>::ok(std::move(commit));
}

}  // namespace auru::pm

#include "auru/pm/types.hpp"

#include <utility>

namespace auru::pm {
namespace {

std::vector<std::string> string_list(const Json* json) {
    std::vector<std::string> values;
    if (json == nullptr) {
        return values;
    }
    for (const Json& element : json->elements()) {
        if (auto text = element.as_text()) {
            values.push_back(*text);
        }
    }
    return values;
}

}  // namespace

// ── Capabilities ─────────────────────────────────────────────────────────────

Capabilities Capabilities::from_json(const Json& json) {
    Capabilities capabilities;
    capabilities.project_listing = json.bool_at("project_listing", false);
    capabilities.members = json.bool_at("members", false);
    capabilities.permissions = json.bool_at("permissions", false);
    capabilities.branches = json.bool_at("branches", false);
    capabilities.server_side_merge = json.bool_at("server_side_merge", false);
    capabilities.compressed_uploads = json.bool_at("compressed_uploads", false);
    capabilities.history_retention = json.bool_at("history_retention", false);
    capabilities.project_scoped_blobs = json.bool_at("project_scoped_blobs", false);
    capabilities.auth_methods = string_list(json.find("auth_methods"));
    return capabilities;
}

// ── OAuth configuration ──────────────────────────────────────────────────────

std::optional<OAuthClient> OAuthConfiguration::client(OAuthClient::Kind kind) const {
    for (const OAuthClient& candidate : clients) {
        if (candidate.kind == kind) {
            return candidate;
        }
    }
    return std::nullopt;
}

OAuthConfiguration OAuthConfiguration::from_json(const Json& json) {
    OAuthConfiguration configuration;
    configuration.issuer = json.optional_string("issuer").value_or("");
    configuration.audience = json.optional_string("audience").value_or("");
    configuration.required_scope = json.optional_string("required_scope").value_or("openid");

    if (const Json* clients = json.find("clients")) {
        for (const Json& entry : clients->elements()) {
            OAuthClient client;
            const std::string kind = entry.optional_string("kind").value_or("native");
            client.kind = kind == "native"    ? OAuthClient::Kind::Native
                          : kind == "browser" ? OAuthClient::Kind::Browser
                          : kind == "mobile"  ? OAuthClient::Kind::Mobile
                                              : OAuthClient::Kind::Unknown;
            client.client_id = entry.optional_string("client_id").value_or("");
            client.redirect_uri = entry.optional_string("redirect_uri").value_or("");
            client.flows = string_list(entry.find("flows"));
            configuration.clients.push_back(std::move(client));
        }
    }

    // The singular fields predate `clients` and describe the native client.
    // Reading them keeps a provider written before the list existed usable.
    if (!configuration.client(OAuthClient::Kind::Native)) {
        auto client_id = json.optional_string("client_id");
        auto redirect_uri = json.optional_string("redirect_uri");
        if (client_id && redirect_uri) {
            OAuthClient native;
            native.kind = OAuthClient::Kind::Native;
            native.client_id = *client_id;
            native.redirect_uri = *redirect_uri;
            native.flows = string_list(json.find("flows"));
            configuration.clients.insert(configuration.clients.begin(), std::move(native));
        }
    }
    return configuration;
}

// ── Health ───────────────────────────────────────────────────────────────────

Result<ProviderHealth> ProviderHealth::from_json(const Json& json) {
    auto protocol = json.string_at("protocol");
    if (!protocol) {
        return Result<ProviderHealth>::fail(protocol.error());
    }

    ProviderHealth health;
    health.protocol = std::move(protocol).value();
    health.provider_id = json.optional_string("provider_id");
    health.name = json.optional_string("name");
    if (const Json* capabilities = json.find("capabilities")) {
        health.capabilities = Capabilities::from_json(*capabilities);
    }
    if (const Json* authentication = json.find("authentication")) {
        health.authentication = OAuthConfiguration::from_json(*authentication);
    }
    return Result<ProviderHealth>::ok(std::move(health));
}

// ── Identity ─────────────────────────────────────────────────────────────────

AuthorIdentity AuthenticatedIdentity::as_author() const {
    AuthorIdentity author;
    author.display_name = display_name;
    author.provider_user_id = user_id;
    author.provider_id = provider_id;
    author.email = email;
    return author;
}

Result<AuthenticatedIdentity> AuthenticatedIdentity::from_json(const Json& json) {
    auto provider_id = json.string_at("provider_id");
    if (!provider_id) {
        return Result<AuthenticatedIdentity>::fail(provider_id.error());
    }
    auto user_id = json.string_at("user_id");
    if (!user_id) {
        return Result<AuthenticatedIdentity>::fail(user_id.error());
    }
    auto display_name = json.string_at("display_name");
    if (!display_name) {
        return Result<AuthenticatedIdentity>::fail(display_name.error());
    }

    AuthenticatedIdentity identity;
    identity.provider_id = std::move(provider_id).value();
    identity.user_id = std::move(user_id).value();
    identity.display_name = std::move(display_name).value();
    identity.email = json.optional_string("email");
    return Result<AuthenticatedIdentity>::ok(std::move(identity));
}

// ── Project profile ──────────────────────────────────────────────────────────

Json ProjectProfile::to_json() const {
    Json metadata = Json::object({});
    if (genre) {
        metadata.set("genre", Json::string(*genre));
    }
    if (!tags.empty()) {
        std::vector<Json> tag_json;
        tag_json.reserve(tags.size());
        for (const std::string& tag : tags) {
            tag_json.push_back(Json::string(tag));
        }
        metadata.set("tags", Json::array(std::move(tag_json)));
    }

    Json json = Json::object({});
    json.set("display_name", Json::string(display_name));
    json.set("format", Json::string(format));
    if (!metadata.members().empty()) {
        json.set("metadata", metadata);
    }
    if (relative_path) {
        Json location = Json::object({});
        location.set("relative_path", Json::string(*relative_path));
        json.set("location", location);
    }
    return json;
}

ProjectProfile ProjectProfile::from_json(const Json& json) {
    ProjectProfile profile;
    profile.display_name = json.optional_string("display_name").value_or("");
    profile.format = json.optional_string("format").value_or("");
    if (const Json* metadata = json.find("metadata")) {
        profile.genre = metadata->optional_string("genre");
        profile.tags = string_list(metadata->find("tags"));
    }
    if (const Json* location = json.find("location")) {
        profile.relative_path = location->optional_string("relative_path");
    }
    return profile;
}

// ── Provider project ─────────────────────────────────────────────────────────

Result<ProviderProject> ProviderProject::from_json(const Json& json) {
    auto handle = json.string_at("handle");
    if (!handle) {
        return Result<ProviderProject>::fail(handle.error());
    }
    auto head_text = json.string_at("head");
    if (!head_text) {
        return Result<ProviderProject>::fail(head_text.error());
    }
    auto head = ContentHash::parse(head_text.value());
    if (!head) {
        return Result<ProviderProject>::fail(head.error());
    }
    auto updated_at = json.integer_at("updated_at");
    if (!updated_at) {
        return Result<ProviderProject>::fail(updated_at.error());
    }

    ProviderProject project;
    project.handle = std::move(handle).value();
    project.head = head.value();
    project.updated_at = updated_at.value();
    if (const Json* profile = json.find("profile")) {
        project.profile = ProjectProfile::from_json(*profile);
    }
    return Result<ProviderProject>::ok(std::move(project));
}

// ── Commit summary ───────────────────────────────────────────────────────────

Result<CommitSummary> CommitSummary::from_json(const Json& json) {
    auto id_text = json.string_at("id");
    if (!id_text) {
        return Result<CommitSummary>::fail(id_text.error());
    }
    auto id = ContentHash::parse(id_text.value());
    if (!id) {
        return Result<CommitSummary>::fail(id.error());
    }

    std::vector<ContentHash> parents;
    if (const Json* parents_json = json.find("parents")) {
        for (const Json& parent : parents_json->elements()) {
            auto hash = ContentHash::parse(parent.as_text().value_or(""));
            if (!hash) {
                return Result<CommitSummary>::fail(hash.error());
            }
            parents.push_back(hash.value());
        }
    }

    const Json* author_json = json.find("author");
    if (author_json == nullptr) {
        return make_error<CommitSummary>(ErrorCode::BadRequest, "a history row has no author");
    }
    auto author = AuthorIdentity::from_json(*author_json);
    if (!author) {
        return Result<CommitSummary>::fail(author.error());
    }
    auto timestamp = json.integer_at("timestamp");
    if (!timestamp) {
        return Result<CommitSummary>::fail(timestamp.error());
    }
    auto message = json.string_at("message");
    if (!message) {
        return Result<CommitSummary>::fail(message.error());
    }

    CommitSummary summary;
    summary.id = id.value();
    summary.parents = std::move(parents);
    summary.author = std::move(author).value();
    summary.timestamp = timestamp.value();
    summary.message = std::move(message).value();
    summary.description = json.optional_string("description").value_or("");
    return Result<CommitSummary>::ok(std::move(summary));
}

// ── Project info ─────────────────────────────────────────────────────────────

std::optional<std::string> ProjectInfo::text(const std::string& name) const {
    const Json* field = detail.find(name);
    return field == nullptr ? std::nullopt : field->as_text();
}

std::optional<double> ProjectInfo::number(const std::string& name) const {
    const Json* field = detail.find(name);
    return field == nullptr ? std::nullopt : field->as_number();
}

Result<ProjectInfo> ProjectInfo::parse(const std::vector<std::uint8_t>& blob) {
    auto json = Json::parse(
        std::string(reinterpret_cast<const char*>(blob.data()), blob.size()));
    if (!json) {
        return Result<ProjectInfo>::fail(json.error());
    }
    return from_json(json.value());
}

Result<ProjectInfo> ProjectInfo::from_json(const Json& json) {
    auto schema = json.integer_at("schema");
    if (!schema) {
        return Result<ProjectInfo>::fail(schema.error());
    }
    auto format = json.string_at("format");
    if (!format) {
        return Result<ProjectInfo>::fail(format.error());
    }

    ProjectInfo info;
    info.schema = schema.value();
    info.format = std::move(format).value();
    for (const char* daw : {"ableton", "flstudio", "dawproject"}) {
        if (const Json* detail = json.find(daw)) {
            info.detail = *detail;
            break;
        }
    }
    return Result<ProjectInfo>::ok(std::move(info));
}

// ── Retention ────────────────────────────────────────────────────────────────

RetentionRule RetentionRule::latest(std::int32_t count) {
    RetentionRule rule;
    rule.policy = Policy::Latest;
    rule.count = count;
    return rule;
}

RetentionRule RetentionRule::since(std::int64_t unix_seconds) {
    RetentionRule rule;
    rule.policy = Policy::Since;
    rule.timestamp = unix_seconds;
    return rule;
}

Json RetentionRequest::to_json() const {
    Json rule_json = Json::object({});
    if (rule.policy == RetentionRule::Policy::Latest) {
        rule_json.set("policy", Json::string("latest"));
        rule_json.set("count", Json::integer(rule.count));
    } else {
        rule_json.set("policy", Json::string("since"));
        rule_json.set("timestamp", Json::integer(rule.timestamp));
    }

    const auto hashes = [](const std::vector<ContentHash>& values) {
        std::vector<Json> json;
        json.reserve(values.size());
        for (const ContentHash& hash : values) {
            json.push_back(Json::string(hash.to_string()));
        }
        return Json::array(std::move(json));
    };

    Json json = Json::object({});
    json.set("rule", rule_json);
    if (!protected_commits.empty()) {
        json.set("protected_commits", hashes(protected_commits));
    }
    if (!protected_blobs.empty()) {
        json.set("protected_blobs", hashes(protected_blobs));
    }
    return json;
}

Result<RetentionReport> RetentionReport::from_json(const Json& json) {
    auto versions = json.integer_at("versions_removed");
    if (!versions) {
        return Result<RetentionReport>::fail(versions.error());
    }
    RetentionReport report;
    report.versions_removed = versions.value();
    report.objects_removed = json.integer_at("objects_removed").value_or(0);
    report.bytes_freed = json.integer_at("bytes_freed").value_or(0);
    return Result<RetentionReport>::ok(report);
}

}  // namespace auru::pm

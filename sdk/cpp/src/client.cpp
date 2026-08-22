#include "auru/pm/client.hpp"

#include <algorithm>
#include <cctype>
#include <mutex>
#include <utility>

namespace auru::pm {
namespace {

/// Percent-encode one path or query segment.
std::string url_encode(const std::string& value) {
    static const char kHex[] = "0123456789ABCDEF";
    std::string encoded;
    encoded.reserve(value.size());
    for (char raw : value) {
        const auto character = static_cast<unsigned char>(raw);
        const bool unreserved = (character >= 'A' && character <= 'Z')
                                || (character >= 'a' && character <= 'z')
                                || (character >= '0' && character <= '9') || character == '-'
                                || character == '_' || character == '.' || character == '~';
        if (unreserved) {
            encoded.push_back(raw);
        } else {
            encoded.push_back('%');
            encoded.push_back(kHex[character >> 4]);
            encoded.push_back(kHex[character & 0x0F]);
        }
    }
    return encoded;
}

Result<std::string> normalize_endpoint(const std::string& endpoint) {
    const auto scheme_end = endpoint.find("://");
    if (scheme_end == std::string::npos) {
        return make_error<std::string>(ErrorCode::BadRequest,
                                       "endpoint is not a URL: " + endpoint);
    }
    const std::string scheme = endpoint.substr(0, scheme_end);
    if (scheme != "http" && scheme != "https") {
        return make_error<std::string>(
            ErrorCode::BadRequest, "endpoint must be http or https, got " + scheme);
    }
    if (endpoint.size() <= scheme_end + 3) {
        return make_error<std::string>(ErrorCode::BadRequest,
                                       "endpoint must name a host: " + endpoint);
    }

    std::string normalized = endpoint;
    while (!normalized.empty() && normalized.back() == '/') {
        normalized.pop_back();
    }
    return Result<std::string>::ok(std::move(normalized));
}

std::vector<std::uint8_t> to_bytes(const std::string& text) {
    return std::vector<std::uint8_t>(text.begin(), text.end());
}

/// Turn a non-2xx response into the right error.
///
/// A provider that invents a code outside the table, or answers with something
/// that is not JSON at all, still has to produce a usable error — a proxy
/// returning an HTML 502 page is the ordinary case, not a hypothetical.
Error error_from(const Response& response) {
    Error error;
    error.status = response.status;

    auto parsed = Json::parse(response.body_text());
    const Json body = parsed.has_value() ? parsed.value() : Json::object({});

    const auto raw_code = body.optional_string("code");
    if (raw_code && *raw_code == "head_conflict") {
        error.code = ErrorCode::HeadConflict;
        error.message = "HEAD moved since it was last read";
        error.current_head = body.optional_string("current");
        return error;
    }

    error.code = raw_code ? error_code_from_wire(*raw_code).value_or(
                                error_code_for_status(response.status))
                          : error_code_for_status(response.status);
    error.message = body.optional_string("message").value_or(
        "HTTP " + std::to_string(response.status) + " from the provider");

    if (auto retry_after = response.header("retry-after")) {
        const bool digits = !retry_after->empty()
                            && std::all_of(retry_after->begin(), retry_after->end(),
                                           [](unsigned char c) { return std::isdigit(c) != 0; });
        if (digits) {
            error.retry_after_seconds = std::stoi(*retry_after);
        }
    }
    return error;
}

}  // namespace

std::optional<std::string> Response::header(const std::string& name) const {
    const auto lower = [](std::string value) {
        std::transform(value.begin(), value.end(), value.begin(),
                       [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
        return value;
    };
    const std::string wanted = lower(name);
    for (const auto& entry : headers) {
        if (lower(entry.first) == wanted) {
            return entry.second;
        }
    }
    return std::nullopt;
}

/// Everything a client and its project clients share.
struct AuruClient::State {
    std::string endpoint;
    std::shared_ptr<Transport> transport;
    ProviderHealth health;

    mutable std::mutex token_mutex;
    std::optional<std::string> access_token;

    Result<Response> send(const std::string& method, const std::string& path,
                          const std::optional<std::string>& json_body,
                          const std::vector<std::uint8_t>* raw_body, const char* accept) const {
        Request request;
        request.method = method;
        request.url = endpoint + path;
        request.headers.emplace_back("accept", accept);

        {
            std::lock_guard<std::mutex> guard(token_mutex);
            if (access_token) {
                request.headers.emplace_back("authorization", "Bearer " + *access_token);
            }
        }

        if (json_body) {
            request.headers.emplace_back("content-type", "application/json");
            request.body = to_bytes(*json_body);
        } else if (raw_body != nullptr) {
            request.headers.emplace_back("content-type", "application/octet-stream");
            request.body = *raw_body;
        }

        auto response = transport->send(request);
        if (!response) {
            return response;
        }
        if (response.value().status / 100 != 2) {
            return Result<Response>::fail(error_from(response.value()));
        }
        return response;
    }

    Result<Json> send_json(const std::string& method, const std::string& path,
                           const std::optional<Json>& body) const {
        std::optional<std::string> encoded;
        if (body) {
            encoded = body->to_json_text();
        }
        auto response = send(method, path, encoded, nullptr, "application/json");
        if (!response) {
            return Result<Json>::fail(response.error());
        }
        if (response.value().body.empty()) {
            return Result<Json>::ok(Json::object({}));
        }
        return Json::parse(response.value().body_text());
    }

    Result<void> require(bool advertised, const char* capability, const char* action) const {
        if (advertised) {
            return Result<void>::ok();
        }
        Error error;
        error.code = ErrorCode::Unsupported;
        error.message = endpoint + " does not support " + action + " (capability " + capability + ")";
        return Result<void>::fail(std::move(error));
    }
};

// ── AuruClient ───────────────────────────────────────────────────────────────

Result<AuruClient> AuruClient::connect(Options options) {
    if (!options.transport) {
        return make_error<AuruClient>(
            ErrorCode::BadRequest,
            "a Transport is required; this library has no built-in HTTP stack");
    }

    auto endpoint = normalize_endpoint(options.endpoint);
    if (!endpoint) {
        return Result<AuruClient>::fail(endpoint.error());
    }

    auto state = std::make_shared<State>();
    state->endpoint = endpoint.value();
    state->transport = std::move(options.transport);
    state->access_token = std::move(options.access_token);

    auto health_json = state->send_json("GET", "/v1/health", std::nullopt);
    if (!health_json) {
        return Result<AuruClient>::fail(health_json.error());
    }
    auto health = ProviderHealth::from_json(health_json.value());
    if (!health) {
        return Result<AuruClient>::fail(health.error());
    }
    if (health.value().protocol != kProtocolVersion) {
        return make_error<AuruClient>(
            ErrorCode::Unsupported,
            state->endpoint + " speaks " + health.value().protocol + "; this client speaks "
                + kProtocolVersion);
    }
    state->health = std::move(health).value();
    return Result<AuruClient>::ok(AuruClient(std::move(state)));
}

const ProviderHealth& AuruClient::health() const { return state_->health; }

const Capabilities& AuruClient::capabilities() const { return state_->health.capabilities; }

const std::string& AuruClient::endpoint() const { return state_->endpoint; }

void AuruClient::set_access_token(std::optional<std::string> token) {
    std::lock_guard<std::mutex> guard(state_->token_mutex);
    state_->access_token = std::move(token);
}

bool AuruClient::authenticated() const {
    std::lock_guard<std::mutex> guard(state_->token_mutex);
    return state_->access_token.has_value();
}

Result<AuthenticatedIdentity> AuruClient::me() const {
    auto json = state_->send_json("GET", "/v1/me", std::nullopt);
    if (!json) {
        return Result<AuthenticatedIdentity>::fail(json.error());
    }
    return AuthenticatedIdentity::from_json(json.value());
}

Result<std::vector<ProviderProject>> AuruClient::list_projects() const {
    auto allowed = state_->require(capabilities().project_listing, "project_listing",
                                   "listing projects");
    if (!allowed) {
        return Result<std::vector<ProviderProject>>::fail(allowed.error());
    }
    auto json = state_->send_json("GET", "/v1/projects", std::nullopt);
    if (!json) {
        return Result<std::vector<ProviderProject>>::fail(json.error());
    }

    std::vector<ProviderProject> projects;
    const Json* listed = json.value().find("projects");
    if (listed != nullptr) {
        for (const Json& entry : listed->elements()) {
            auto project = ProviderProject::from_json(entry);
            if (!project) {
                return Result<std::vector<ProviderProject>>::fail(project.error());
            }
            projects.push_back(std::move(project).value());
        }
    }
    return Result<std::vector<ProviderProject>>::ok(std::move(projects));
}

Result<ProjectClient> AuruClient::project(std::string handle) const {
    if (handle.empty()) {
        return make_error<ProjectClient>(ErrorCode::BadRequest,
                                         "a project handle must not be empty");
    }
    return Result<ProjectClient>::ok(ProjectClient(state_, std::move(handle)));
}

// ── ProjectClient ────────────────────────────────────────────────────────────

ProjectClient::ProjectClient(std::shared_ptr<AuruClient::State> state, std::string handle)
    : state_(std::move(state)),
      handle_(std::move(handle)),
      base_("/v1/projects/" + url_encode(handle_)) {}

Result<void> ProjectClient::put_profile(const ProjectProfile& profile) const {
    auto allowed = state_->require(state_->health.capabilities.project_listing, "project_listing",
                                   "project profiles");
    if (!allowed) {
        return allowed;
    }
    auto json = state_->send_json("PUT", base_, profile.to_json());
    if (!json) {
        return Result<void>::fail(json.error());
    }
    return Result<void>::ok();
}

Result<std::optional<ContentHash>> ProjectClient::head() const {
    auto json = state_->send_json("GET", base_ + "/head", std::nullopt);
    if (!json) {
        return Result<std::optional<ContentHash>>::fail(json.error());
    }
    auto text = json.value().optional_string("commit_id");
    if (!text) {
        return Result<std::optional<ContentHash>>::ok(std::nullopt);
    }
    auto hash = ContentHash::parse(*text);
    if (!hash) {
        return Result<std::optional<ContentHash>>::fail(hash.error());
    }
    return Result<std::optional<ContentHash>>::ok(hash.value());
}

Result<void> ProjectClient::advance_head(const std::optional<ContentHash>& from,
                                         const ContentHash& to) const {
    Json body = Json::object({});
    body.set("from", from ? Json::string(from->to_string()) : Json::null());
    body.set("to", Json::string(to.to_string()));

    auto json = state_->send_json("POST", base_ + "/head", body);
    if (!json) {
        return Result<void>::fail(json.error());
    }
    return Result<void>::ok();
}

Result<ContentHash> ProjectClient::put_commit(const Commit& commit) const {
    auto json = state_->send_json("POST", base_ + "/commits", commit.to_json());
    if (!json) {
        return Result<ContentHash>::fail(json.error());
    }
    auto id = json.value().string_at("id");
    if (!id) {
        return Result<ContentHash>::fail(id.error());
    }
    return ContentHash::parse(id.value());
}

Result<Commit> ProjectClient::get_commit(const ContentHash& id) const {
    auto json = state_->send_json("GET", base_ + "/commits/" + url_encode(id.to_string()),
                                  std::nullopt);
    if (!json) {
        return Result<Commit>::fail(json.error());
    }
    return Commit::from_json(json.value());
}

Result<std::vector<CommitSummary>> ProjectClient::history(const HistoryRange& range) const {
    std::string query;
    if (range.limit > 0) {
        query = "limit=" + std::to_string(range.limit);
    }
    if (range.before) {
        if (!query.empty()) {
            query += "&";
        }
        query += "before=" + url_encode(range.before->to_string());
    }

    const std::string path = base_ + "/history" + (query.empty() ? "" : "?" + query);
    auto json = state_->send_json("GET", path, std::nullopt);
    if (!json) {
        return Result<std::vector<CommitSummary>>::fail(json.error());
    }

    std::vector<CommitSummary> commits;
    const Json* listed = json.value().find("commits");
    if (listed != nullptr) {
        for (const Json& entry : listed->elements()) {
            auto summary = CommitSummary::from_json(entry);
            if (!summary) {
                return Result<std::vector<CommitSummary>>::fail(summary.error());
            }
            commits.push_back(std::move(summary).value());
        }
    }
    return Result<std::vector<CommitSummary>>::ok(std::move(commits));
}

Result<RetentionReport> ProjectClient::prune_history(const RetentionRequest& request) const {
    auto allowed = state_->require(state_->health.capabilities.history_retention,
                                   "history_retention", "history retention");
    if (!allowed) {
        return Result<RetentionReport>::fail(allowed.error());
    }
    auto json = state_->send_json("POST", base_ + "/retention", request.to_json());
    if (!json) {
        return Result<RetentionReport>::fail(json.error());
    }
    return RetentionReport::from_json(json.value());
}

Result<std::vector<bool>> ProjectClient::has_blobs(const std::vector<ContentHash>& hashes) const {
    if (hashes.empty()) {
        return Result<std::vector<bool>>::ok({});
    }
    std::vector<Json> hash_json;
    hash_json.reserve(hashes.size());
    for (const ContentHash& hash : hashes) {
        hash_json.push_back(Json::string(hash.to_string()));
    }
    Json body = Json::object({});
    body.set("hashes", Json::array(std::move(hash_json)));

    auto json = state_->send_json("POST", base_ + "/blobs/has", body);
    if (!json) {
        return Result<std::vector<bool>>::fail(json.error());
    }

    std::vector<bool> present;
    const Json* listed = json.value().find("present");
    if (listed != nullptr) {
        for (const Json& flag : listed->elements()) {
            present.push_back(flag.as_bool().value_or(false));
        }
    }
    return Result<std::vector<bool>>::ok(std::move(present));
}

Result<ContentHash> ProjectClient::put_blob(const std::vector<std::uint8_t>& bytes) const {
    const ContentHash hash = ContentHash::of(bytes.data(), bytes.size());
    auto response = state_->send("PUT", base_ + "/blobs/" + url_encode(hash.to_string()),
                                 std::nullopt, &bytes, "application/json");
    if (!response) {
        return Result<ContentHash>::fail(response.error());
    }
    return Result<ContentHash>::ok(hash);
}

Result<std::vector<std::uint8_t>> ProjectClient::get_blob_unverified(
    const ContentHash& hash) const {
    auto response = state_->send("GET", base_ + "/blobs/" + url_encode(hash.to_string()),
                                 std::nullopt, nullptr, "application/octet-stream");
    if (!response) {
        return Result<std::vector<std::uint8_t>>::fail(response.error());
    }
    return Result<std::vector<std::uint8_t>>::ok(std::move(response).value().body);
}

Result<std::vector<std::uint8_t>> ProjectClient::get_blob(const ContentHash& hash) const {
    auto bytes = get_blob_unverified(hash);
    if (!bytes) {
        return bytes;
    }
    if (!hash.matches(bytes.value().data(), bytes.value().size())) {
        return make_error<std::vector<std::uint8_t>>(
            ErrorCode::BadRequest,
            "blob " + hash.to_string()
                + " does not hash to its own name; the provider returned different bytes");
    }
    return bytes;
}

Result<std::optional<ProjectInfo>> ProjectClient::project_info(const Commit& commit) const {
    if (!commit.metadata()) {
        return Result<std::optional<ProjectInfo>>::ok(std::nullopt);
    }
    auto blob = get_blob(*commit.metadata());
    if (!blob) {
        return Result<std::optional<ProjectInfo>>::fail(blob.error());
    }
    auto info = ProjectInfo::parse(blob.value());
    if (!info) {
        return Result<std::optional<ProjectInfo>>::fail(info.error());
    }
    return Result<std::optional<ProjectInfo>>::ok(std::move(info).value());
}

}  // namespace auru::pm

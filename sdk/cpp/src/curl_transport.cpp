#include "auru/pm/curl_transport.hpp"

#include <curl/curl.h>

#include <algorithm>
#include <cctype>
#include <string>
#include <utility>

namespace auru::pm {
namespace {

std::size_t collect_body(char* data, std::size_t size, std::size_t count, void* userdata) {
    auto* body = static_cast<std::vector<std::uint8_t>*>(userdata);
    const std::size_t total = size * count;
    body->insert(body->end(), data, data + total);
    return total;
}

std::size_t collect_header(char* data, std::size_t size, std::size_t count, void* userdata) {
    auto* headers = static_cast<Headers*>(userdata);
    const std::size_t total = size * count;
    std::string line(data, total);

    while (!line.empty() && (line.back() == '\r' || line.back() == '\n')) {
        line.pop_back();
    }
    const auto colon = line.find(':');
    if (colon != std::string::npos) {
        std::string value = line.substr(colon + 1);
        while (!value.empty() && value.front() == ' ') {
            value.erase(value.begin());
        }
        headers->emplace_back(line.substr(0, colon), value);
    }
    return total;
}

/// libcurl is initialized once per process.
///
/// `curl_global_init` is not thread-safe and must not run concurrently with any
/// other curl call, so it happens before `main` reaches anything that could.
struct GlobalInit {
    GlobalInit() { curl_global_init(CURL_GLOBAL_DEFAULT); }
    ~GlobalInit() { curl_global_cleanup(); }
};

}  // namespace

CurlTransport::CurlTransport() : CurlTransport(Options{}) {}

CurlTransport::CurlTransport(Options options) : options_(std::move(options)) {
    static GlobalInit global;
    static_cast<void>(&global);
}

CurlTransport::~CurlTransport() = default;

Result<Response> CurlTransport::send(const Request& request) {
    CURL* handle = curl_easy_init();
    if (handle == nullptr) {
        return make_error<Response>(ErrorCode::Internal, "cannot create a curl handle");
    }

    Response response;
    curl_slist* headers = nullptr;
    for (const auto& header : request.headers) {
        headers = curl_slist_append(headers, (header.first + ": " + header.second).c_str());
    }
    // curl adds this itself and would otherwise wait for a 100-Continue that
    // never comes, which reads as a hang rather than as a mistake.
    headers = curl_slist_append(headers, "Expect:");

    curl_easy_setopt(handle, CURLOPT_URL, request.url.c_str());
    curl_easy_setopt(handle, CURLOPT_CUSTOMREQUEST, request.method.c_str());
    curl_easy_setopt(handle, CURLOPT_HTTPHEADER, headers);
    curl_easy_setopt(handle, CURLOPT_WRITEFUNCTION, collect_body);
    curl_easy_setopt(handle, CURLOPT_WRITEDATA, &response.body);
    curl_easy_setopt(handle, CURLOPT_HEADERFUNCTION, collect_header);
    curl_easy_setopt(handle, CURLOPT_HEADERDATA, &response.headers);
    curl_easy_setopt(handle, CURLOPT_TIMEOUT, options_.timeout_seconds);
    curl_easy_setopt(handle, CURLOPT_CONNECTTIMEOUT, options_.connect_timeout_seconds);
    curl_easy_setopt(handle, CURLOPT_USERAGENT, options_.user_agent.c_str());
    curl_easy_setopt(handle, CURLOPT_NOSIGNAL, 1L);
    // Redirects are not followed: this protocol has none, and following one
    // would resend a bearer token to wherever the redirect pointed.
    curl_easy_setopt(handle, CURLOPT_FOLLOWLOCATION, 0L);

    if (!options_.ca_bundle_path.empty()) {
        curl_easy_setopt(handle, CURLOPT_CAINFO, options_.ca_bundle_path.c_str());
    }

    if (!request.body.empty()) {
        curl_easy_setopt(handle, CURLOPT_POSTFIELDS,
                         reinterpret_cast<const char*>(request.body.data()));
        curl_easy_setopt(handle, CURLOPT_POSTFIELDSIZE,
                         static_cast<long>(request.body.size()));
    }

    const CURLcode outcome = curl_easy_perform(handle);
    if (outcome != CURLE_OK) {
        const std::string message = curl_easy_strerror(outcome);
        curl_slist_free_all(headers);
        curl_easy_cleanup(handle);
        return make_error<Response>(ErrorCode::Internal,
                                    request.method + " " + request.url + ": " + message);
    }

    long status = 0;
    curl_easy_getinfo(handle, CURLINFO_RESPONSE_CODE, &status);
    response.status = static_cast<int>(status);

    curl_slist_free_all(headers);
    curl_easy_cleanup(handle);
    return Result<Response>::ok(std::move(response));
}

}  // namespace auru::pm

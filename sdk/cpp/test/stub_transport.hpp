#pragma once

#include <functional>
#include <string>
#include <vector>

#include "auru/pm/transport.hpp"

/// A transport that answers whatever a test tells it to.
///
/// Providers are written by third parties, so the interesting cases are the
/// ones outside the table — an invented code, or a proxy answering with an HTML
/// error page. Those are ordinary, not hypothetical, and only a stub can
/// produce them on demand.
class StubTransport final : public auru::pm::Transport {
public:
    using Handler = std::function<auru::pm::Response(const auru::pm::Request&)>;

    explicit StubTransport(Handler handler) : handler_(std::move(handler)) {}

    auru::pm::Result<auru::pm::Response> send(const auru::pm::Request& request) override {
        requests.push_back(request);
        return auru::pm::Result<auru::pm::Response>::ok(handler_(request));
    }

    std::vector<auru::pm::Request> requests;

    static auru::pm::Response json(int status, const std::string& body,
                                   const auru::pm::Headers& headers = {}) {
        auru::pm::Response response;
        response.status = status;
        response.headers = headers;
        response.headers.emplace_back("content-type", "application/json");
        response.body.assign(body.begin(), body.end());
        return response;
    }

    static auru::pm::Response raw(int status, const std::string& body) {
        auru::pm::Response response;
        response.status = status;
        response.body.assign(body.begin(), body.end());
        return response;
    }

private:
    Handler handler_;
};

inline const char* kStubHealth =
    "{\"protocol\":\"auru-pm-v1\",\"provider_id\":\"stub\",\"capabilities\":"
    "{\"project_listing\":true,\"members\":false,\"permissions\":false,\"branches\":false,"
    "\"server_side_merge\":false,\"history_retention\":false,\"project_scoped_blobs\":true,"
    "\"auth_methods\":[\"none\"]}}";

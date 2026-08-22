#pragma once

#include <memory>
#include <string>

#include "auru/pm/transport.hpp"

namespace auru::pm {

/// A `Transport` over libcurl.
///
/// Optional: build with `-DAURU_PM_WITH_CURL=ON` and link `auru::pm_curl`. It
/// lives in its own target so that linking the core never drags libcurl in —
/// most applications that would use this library already have an HTTP stack,
/// and a second one means a second TLS configuration and a second set of
/// certificates to keep current.
///
/// Each call uses its own easy handle, so instances are safe to share between
/// threads.
class CurlTransport final : public Transport {
public:
    struct Options {
        /// Whole-request timeout. Generous by default because a blob can be
        /// large; lower it for an interactive path where a stall is worse than
        /// a failure.
        long timeout_seconds = 60;
        long connect_timeout_seconds = 10;
        /// A CA bundle, when the system store is not the right one.
        std::string ca_bundle_path;
        /// Sent as `User-Agent`.
        std::string user_agent = "auru-pm-cpp/0.1.0";
    };

    // Two constructors rather than a defaulted argument: a default of `{}`
    // would need `Options` complete inside its own enclosing class definition.
    CurlTransport();
    explicit CurlTransport(Options options);
    ~CurlTransport() override;

    CurlTransport(const CurlTransport&) = delete;
    CurlTransport& operator=(const CurlTransport&) = delete;

    Result<Response> send(const Request& request) override;

private:
    Options options_;
};

}  // namespace auru::pm

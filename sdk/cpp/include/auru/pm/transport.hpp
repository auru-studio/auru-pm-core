#pragma once

#include <cstdint>
#include <memory>
#include <optional>
#include <string>
#include <utility>
#include <vector>

#include "auru/pm/result.hpp"

namespace auru::pm {

using Headers = std::vector<std::pair<std::string, std::string>>;

struct Request {
    std::string method;
    std::string url;
    Headers headers;
    std::vector<std::uint8_t> body;
};

struct Response {
    int status = 0;
    Headers headers;
    std::vector<std::uint8_t> body;

    /// A header by name, matched case-insensitively as HTTP requires.
    std::optional<std::string> header(const std::string& name) const;

    std::string body_text() const {
        return std::string(reinterpret_cast<const char*>(body.data()), body.size());
    }
};

/// How this client reaches a provider.
///
/// An interface rather than a built-in HTTP stack, because C++ has no standard
/// one and most applications that would use this library already have made that
/// choice. Forcing a second stack on them — with a second TLS configuration, a
/// second proxy setting, and a second set of certificates to keep current —
/// would be the wrong trade for saving a caller twenty lines.
///
/// An adapter over libcurl ships alongside; see `auru/pm/curl_transport.hpp`.
///
/// Implementations must be safe to call from multiple threads if the client is
/// shared between them.
class Transport {
public:
    virtual ~Transport() = default;

    /// Perform one request.
    ///
    /// Return an `Error` only when the request could not be completed at all —
    /// DNS, connection, TLS, timeout. A non-2xx response is a `Response`, not an
    /// error: interpreting a provider's status is this library's job, and an
    /// adapter that guessed would have to be corrected here anyway.
    virtual Result<Response> send(const Request& request) = 0;
};

}  // namespace auru::pm

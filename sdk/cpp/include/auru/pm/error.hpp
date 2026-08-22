#pragma once

#include <optional>
#include <string>

namespace auru::pm {

/// The closed set of error codes a provider may return.
///
/// Closed on purpose: a caller can switch over it and the compiler will say when
/// a case is missing. A provider that invents a code outside the table is not
/// conformant; when one arrives anyway it is mapped by HTTP status instead.
enum class ErrorCode {
    BadRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    HeadConflict,
    Unsupported,
    RateLimited,
    StorageError,
    Internal,
    /// The identity provider was unreachable, so the token was never judged.
    ///
    /// Distinct from Unauthorized because the right response is to retry rather
    /// than to prompt someone to sign in again.
    AuthenticationUnavailable,
};

/// The value as it appears in a provider's JSON body.
const char* wire_value(ErrorCode code) noexcept;

/// Parse the wire form. Empty for anything outside the table.
std::optional<ErrorCode> error_code_from_wire(const std::string& value) noexcept;

/// The fallback when a provider sends no usable code — a proxy's HTML 502, say.
ErrorCode error_code_for_status(int status) noexcept;

/// Whether retrying the identical request could succeed.
bool retryable(ErrorCode code) noexcept;

/// Something a provider refused, or that this client refused before sending.
struct Error {
    ErrorCode code = ErrorCode::Internal;
    std::string message;
    /// The HTTP status, absent when the client refused the call before sending.
    std::optional<int> status;
    /// Seconds to wait, from `Retry-After`. Only ever set on a rate limit.
    std::optional<int> retry_after_seconds;
    /// The provider's actual HEAD, set only on HeadConflict.
    ///
    /// Carried on the error rather than fetched afterwards so a client that lost
    /// a compare-and-swap can rebase without another round trip.
    std::optional<std::string> current_head;

    bool retryable() const noexcept { return ::auru::pm::retryable(code); }

    /// A single line suitable for a log.
    std::string to_string() const;
};

}  // namespace auru::pm

#pragma once

/// Throwing equivalents, for callers that build with exceptions enabled.
///
/// The core returns `Result<T>` because a large part of this library's audience
/// — audio plugins, embedded and real-time builds — compiles with
/// `-fno-exceptions`, and an API they cannot use is not an API. Everyone else
/// pays for that in verbosity at every call site, so this header buys it back:
///
/// ```cpp
/// using auru::pm::unwrap;
/// auto client = unwrap(AuruClient::connect({endpoint, transport, token}));
/// auto head   = unwrap(unwrap(client.project("song")).head());
/// ```
///
/// Including this header is opt-in. Nothing in the core depends on it, and it
/// compiles to nothing when exceptions are off.

#if defined(__cpp_exceptions) || defined(_CPPUNWIND) || defined(__EXCEPTIONS)
#define AURU_PM_HAS_EXCEPTIONS 1
#else
#define AURU_PM_HAS_EXCEPTIONS 0
#endif

#if AURU_PM_HAS_EXCEPTIONS

#include <stdexcept>
#include <utility>

#include "auru/pm/result.hpp"

namespace auru::pm {

/// An `Error` raised as an exception.
class Exception : public std::runtime_error {
public:
    explicit Exception(Error error)
        : std::runtime_error(error.to_string()), error_(std::move(error)) {}

    const Error& error() const noexcept { return error_; }
    ErrorCode code() const noexcept { return error_.code; }
    bool retryable() const noexcept { return error_.retryable(); }

private:
    Error error_;
};

/// The value, or an `Exception` carrying the error.
template <typename T>
T unwrap(Result<T>&& result) {
    if (!result) {
        throw Exception(result.error());
    }
    return std::move(result).value();
}

/// As above, for a result that carries no value.
inline void unwrap(Result<void>&& result) {
    if (!result) {
        throw Exception(result.error());
    }
}

}  // namespace auru::pm

#endif  // AURU_PM_HAS_EXCEPTIONS

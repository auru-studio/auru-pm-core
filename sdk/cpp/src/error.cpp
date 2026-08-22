#include "auru/pm/error.hpp"

#include <array>
#include <utility>

namespace auru::pm {
namespace {

constexpr std::array<std::pair<ErrorCode, const char*>, 10> kWireValues = {{
    {ErrorCode::BadRequest, "bad_request"},
    {ErrorCode::Unauthorized, "unauthorized"},
    {ErrorCode::Forbidden, "forbidden"},
    {ErrorCode::NotFound, "not_found"},
    {ErrorCode::HeadConflict, "head_conflict"},
    {ErrorCode::Unsupported, "unsupported"},
    {ErrorCode::RateLimited, "rate_limited"},
    {ErrorCode::StorageError, "storage_error"},
    {ErrorCode::Internal, "internal"},
    {ErrorCode::AuthenticationUnavailable, "authentication_unavailable"},
}};

}  // namespace

const char* wire_value(ErrorCode code) noexcept {
    for (const auto& entry : kWireValues) {
        if (entry.first == code) {
            return entry.second;
        }
    }
    return "internal";
}

std::optional<ErrorCode> error_code_from_wire(const std::string& value) noexcept {
    for (const auto& entry : kWireValues) {
        if (value == entry.second) {
            return entry.first;
        }
    }
    return std::nullopt;
}

ErrorCode error_code_for_status(int status) noexcept {
    switch (status) {
        case 400: return ErrorCode::BadRequest;
        case 401: return ErrorCode::Unauthorized;
        case 403: return ErrorCode::Forbidden;
        case 404: return ErrorCode::NotFound;
        case 409: return ErrorCode::HeadConflict;
        case 422: return ErrorCode::Unsupported;
        case 429: return ErrorCode::RateLimited;
        case 503: return ErrorCode::AuthenticationUnavailable;
        default: return status >= 500 ? ErrorCode::Internal : ErrorCode::BadRequest;
    }
}

bool retryable(ErrorCode code) noexcept {
    return code == ErrorCode::RateLimited || code == ErrorCode::StorageError
           || code == ErrorCode::Internal || code == ErrorCode::AuthenticationUnavailable;
}

std::string Error::to_string() const {
    std::string text = std::string(wire_value(code)) + ": " + message;
    if (status) {
        text += " (HTTP " + std::to_string(*status) + ")";
    }
    return text;
}

}  // namespace auru::pm

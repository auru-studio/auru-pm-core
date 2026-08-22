#pragma once

#include <cassert>
#include <string>
#include <type_traits>
#include <utility>
#include <variant>

#include "auru/pm/error.hpp"

namespace auru::pm {

/// Either a value or an `Error`.
///
/// The core API returns these rather than throwing, because a large part of the
/// audience — audio plugins, embedded builds — compiles with `-fno-exceptions`.
/// `auru/pm/throwing.hpp` adds throwing equivalents for everyone else.
///
/// Checking is not optional in practice: `value()` on a failed result asserts in
/// a debug build, and the type has no implicit conversion that would let a
/// caller skip the check by accident.
template <typename T>
class Result {
public:
    Result(T value) : storage_(std::move(value)) {}   // NOLINT(*-explicit-constructor)
    Result(Error error) : storage_(std::move(error)) {}  // NOLINT(*-explicit-constructor)

    static Result ok(T value) { return Result(std::move(value)); }
    static Result fail(Error error) { return Result(std::move(error)); }

    bool has_value() const noexcept { return storage_.index() == 0; }
    explicit operator bool() const noexcept { return has_value(); }

    const T& value() const& {
        assert(has_value() && "Result::value() on a failed result");
        return std::get<0>(storage_);
    }

    /// The value, mutable.
    ///
    /// Without this overload a non-const `Result` still yields a `const T&`,
    /// because the const-qualified overload is the only lvalue candidate — and
    /// the error is a confusing one about `this` being const on a method that
    /// looks entirely reachable.
    T& value() & {
        assert(has_value() && "Result::value() on a failed result");
        return std::get<0>(storage_);
    }

    T&& value() && {
        assert(has_value() && "Result::value() on a failed result");
        return std::get<0>(std::move(storage_));
    }

    /// The value, or `fallback` when this failed.
    T value_or(T fallback) const& {
        return has_value() ? std::get<0>(storage_) : std::move(fallback);
    }

    const Error& error() const& {
        assert(!has_value() && "Result::error() on a successful result");
        return std::get<1>(storage_);
    }

private:
    std::variant<T, Error> storage_;
};

/// A call that either succeeded or did not.
template <>
class Result<void> {
public:
    Result() = default;
    Result(Error error) : error_(std::move(error)), failed_(true) {}  // NOLINT(*-explicit-constructor)

    static Result ok() { return Result(); }
    static Result fail(Error error) { return Result(std::move(error)); }

    bool has_value() const noexcept { return !failed_; }
    explicit operator bool() const noexcept { return has_value(); }

    const Error& error() const& {
        assert(failed_ && "Result::error() on a successful result");
        return error_;
    }

private:
    Error error_;
    bool failed_ = false;
};

/// Build a failed result of any type.
template <typename T = void>
Result<T> make_error(ErrorCode code, std::string message) {
    Error error;
    error.code = code;
    error.message = std::move(message);
    return Result<T>::fail(std::move(error));
}

}  // namespace auru::pm

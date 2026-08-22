#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <utility>
#include <vector>

#include "auru/pm/result.hpp"

namespace auru::pm {

/// A JSON value, and the canonical encoding a commit id is derived from.
///
/// Written here rather than taken from a dependency for two reasons. A client
/// library that pulls in a JSON stack causes version conflicts in somebody
/// else's build. More importantly, canonical encoding needs exact control over
/// the output: a third-party serializer's formatting choices would become this
/// library's bug, and it would surface as a provider rejecting a commit rather
/// than as anything that names the cause.
///
/// Integers and non-integers are separate kinds because RFC 8785 treats them
/// differently and only one of them can be canonicalized here — see
/// `to_canonical_json`.
class Json {
public:
    enum class Kind { Null, Bool, Int, Dec, Str, Arr, Obj };

    /// Members, in insertion order. Canonical output sorts them.
    using Members = std::vector<std::pair<std::string, Json>>;

    Json() = default;

    static Json null();
    static Json boolean(bool value);
    static Json integer(std::int64_t value);
    static Json number(double value);
    static Json string(std::string value);
    static Json array(std::vector<Json> elements);
    static Json object(Members members);

    Kind kind() const noexcept { return kind_; }
    bool is_null() const noexcept { return kind_ == Kind::Null; }

    /// This value as text, empty when it is not a string.
    std::optional<std::string> as_text() const;
    /// This value as a whole number, empty when it is not one.
    std::optional<std::int64_t> as_integer() const;
    /// This value as a number, empty when it is not one. Integers widen.
    std::optional<double> as_number() const;
    /// This value as a boolean, empty when it is not one.
    std::optional<bool> as_bool() const;

    /// A member by name. Empty when missing, JSON null, or not an object.
    ///
    /// JSON null reads as absent so a caller never has to distinguish "missing"
    /// from "explicitly null" when neither carries information.
    const Json* find(const std::string& name) const;

    /// A required member, as text.
    Result<std::string> string_at(const std::string& name) const;
    /// An optional member, as text.
    std::optional<std::string> optional_string(const std::string& name) const;
    /// A required member, as a whole number.
    Result<std::int64_t> integer_at(const std::string& name) const;
    /// An optional boolean member, with a fallback.
    bool bool_at(const std::string& name, bool fallback) const;

    /// Elements, empty when this is not an array.
    const std::vector<Json>& elements() const noexcept { return elements_; }
    /// Members, empty when this is not an object.
    const Members& members() const noexcept { return members_; }

    /// Add or replace a member. A null value is written; use `set_if` to omit.
    void set(std::string name, Json value);
    /// Add a member only when `value` holds something.
    void set_if(std::string name, std::optional<Json> value);

    /// RFC 8785 canonical JSON — the bytes a commit id is the BLAKE3 of.
    ///
    /// Fails on a non-integral number, or an integer outside +/-(2^53 - 1). RFC
    /// 8785 defines numbers by ECMAScript's `Number::toString`, which is
    /// IEEE-754 binary64; producing approximate bytes would mean producing a
    /// commit id no other implementation agrees with, so this refuses instead.
    /// Nothing in a commit is fractional, and every integer in one is far
    /// inside the bound.
    Result<std::string> to_canonical_json() const;

    /// Ordinary JSON text, for a request body.
    ///
    /// Keeps insertion order, which reads better in a log, and can write a
    /// fractional number. Nothing derives a hash from these bytes.
    std::string to_json_text() const;

    /// Parse JSON text.
    static Result<Json> parse(const std::string& text);

    /// The largest integer RFC 8785 can represent exactly.
    static constexpr std::int64_t kMaxSafeInteger = 9007199254740991LL;

private:
    Kind kind_ = Kind::Null;
    bool bool_ = false;
    std::int64_t int_ = 0;
    double dec_ = 0.0;
    std::string string_;
    std::vector<Json> elements_;
    Members members_;
};

}  // namespace auru::pm

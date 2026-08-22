#pragma once

#include <array>
#include <cstddef>
#include <cstdint>
#include <string>

#include "auru/pm/result.hpp"

namespace auru::pm {

/// A BLAKE3 content hash, in the canonical `blake3:<64 lowercase hex>` form.
///
/// Every hash on the wire uses this shape, commit ids included. The type exists
/// so a hash and an arbitrary string cannot be confused at a call site.
class ContentHash {
public:
    /// The hash of `data`.
    static ContentHash of(const void* data, std::size_t length);
    static ContentHash of(const std::string& text);

    /// Parse the canonical string form.
    ///
    /// Uppercase hex is rejected rather than normalized: two spellings of one
    /// hash would compare unequal as strings, and something downstream
    /// eventually compares them as strings.
    static Result<ContentHash> parse(const std::string& text);

    /// Whether `data` hashes to this. The check a client owes itself on a
    /// download.
    bool matches(const void* data, std::size_t length) const;

    const std::array<std::uint8_t, 32>& bytes() const noexcept { return bytes_; }

    std::string to_string() const;

    bool operator==(const ContentHash& other) const noexcept { return bytes_ == other.bytes_; }
    bool operator!=(const ContentHash& other) const noexcept { return !(*this == other); }
    bool operator<(const ContentHash& other) const noexcept { return bytes_ < other.bytes_; }

private:
    explicit ContentHash(std::array<std::uint8_t, 32> bytes) : bytes_(bytes) {}

    std::array<std::uint8_t, 32> bytes_{};
};

}  // namespace auru::pm

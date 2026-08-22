#include "auru/pm/content_hash.hpp"

#include <cctype>

#include "blake3.hpp"

namespace auru::pm {
namespace {

constexpr char kHex[] = "0123456789abcdef";
constexpr char kPrefix[] = "blake3:";
constexpr std::size_t kPrefixLength = 7;

bool is_lowercase_hex(char character) noexcept {
    return (character >= '0' && character <= '9') || (character >= 'a' && character <= 'f');
}

std::uint8_t hex_value(char character) noexcept {
    return static_cast<std::uint8_t>(character <= '9' ? character - '0' : character - 'a' + 10);
}

}  // namespace

ContentHash ContentHash::of(const void* data, std::size_t length) {
    return ContentHash(detail::Blake3::hash(data, length));
}

ContentHash ContentHash::of(const std::string& text) {
    return of(text.data(), text.size());
}

Result<ContentHash> ContentHash::parse(const std::string& text) {
    const auto reject = [&text] {
        return make_error<ContentHash>(
            ErrorCode::BadRequest,
            "not a canonical content hash: \"" + text + "\" (expected blake3:<64 lowercase hex>)");
    };

    if (text.size() != kPrefixLength + 64 || text.compare(0, kPrefixLength, kPrefix) != 0) {
        return reject();
    }

    std::array<std::uint8_t, 32> bytes{};
    for (std::size_t index = 0; index < 32; ++index) {
        const char high = text[kPrefixLength + index * 2];
        const char low = text[kPrefixLength + index * 2 + 1];
        // Uppercase is rejected rather than normalized: two spellings of one
        // hash would compare unequal as strings, and something downstream
        // eventually compares them as strings.
        if (!is_lowercase_hex(high) || !is_lowercase_hex(low)) {
            return reject();
        }
        bytes[index] = static_cast<std::uint8_t>((hex_value(high) << 4) | hex_value(low));
    }
    return ContentHash(bytes);
}

bool ContentHash::matches(const void* data, std::size_t length) const {
    return detail::Blake3::hash(data, length) == bytes_;
}

std::string ContentHash::to_string() const {
    std::string text;
    text.reserve(kPrefixLength + 64);
    text.append(kPrefix);
    for (std::uint8_t value : bytes_) {
        text.push_back(kHex[value >> 4]);
        text.push_back(kHex[value & 0x0f]);
    }
    return text;
}

}  // namespace auru::pm

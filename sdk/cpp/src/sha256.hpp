#pragma once

#include <array>
#include <cstddef>
#include <cstdint>

namespace auru::pm::detail {

/// SHA-256, for the PKCE `S256` code challenge.
///
/// Implemented here for the same reason BLAKE3 is: this library has no
/// dependencies, and an OpenSSL dependency for one hash would be by far the
/// heaviest thing in the build. It is used only to derive a code challenge from
/// a locally generated verifier — never to protect anything at rest.
std::array<std::uint8_t, 32> sha256(const void* data, std::size_t length);

}  // namespace auru::pm::detail

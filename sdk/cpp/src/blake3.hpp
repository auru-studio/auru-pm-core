#pragma once

#include <array>
#include <cstddef>
#include <cstdint>

namespace auru::pm::detail {

/// BLAKE3, as used for every hash in `auru-pm-v1`.
///
/// Implemented here rather than taken from a dependency because this library has
/// no dependencies, and because a hash is the one thing that can be verified
/// completely: `spec/vectors/content-hash.json` covers every length where
/// BLAKE3's structure changes — the 64-byte compression block, the 1024-byte
/// chunk, and each point where the Merkle tree gains a level.
///
/// Only the plain hash is implemented. Keyed hashing and key derivation are part
/// of BLAKE3 but no part of this protocol, and code that is never exercised is
/// code that is never correct.
class Blake3 {
public:
    Blake3();

    void update(const void* data, std::size_t length);
    std::array<std::uint8_t, 32> finish() const;

    static std::array<std::uint8_t, 32> hash(const void* data, std::size_t length);

private:
    static constexpr std::size_t kBlockLen = 64;
    static constexpr std::size_t kChunkLen = 1024;

    /// A node's compression inputs, kept unevaluated.
    ///
    /// Whether a node is the root is only known once the input ends, and the
    /// root is compressed with an extra flag — so a node's chaining value and
    /// its root output are different results from the same inputs, and both
    /// have to stay reachable.
    struct Output {
        std::array<std::uint32_t, 8> input_chaining_value{};
        std::array<std::uint32_t, 16> block_words{};
        std::uint64_t counter = 0;
        std::uint32_t block_len = 0;
        std::uint32_t flags = 0;

        std::array<std::uint32_t, 8> chaining_value() const;
        std::array<std::uint8_t, 32> root_bytes() const;
    };

    /// One 1024-byte chunk, accumulated a block at a time.
    struct ChunkState {
        std::array<std::uint32_t, 8> chaining_value{};
        std::uint64_t chunk_counter = 0;
        std::array<std::uint8_t, kBlockLen> block{};
        std::uint32_t block_len = 0;
        std::uint32_t blocks_compressed = 0;

        explicit ChunkState(std::uint64_t counter);
        std::size_t length() const noexcept;
        std::uint32_t start_flag() const noexcept;
        void update(const std::uint8_t* input, std::size_t length);
        Output output() const;
    };

    void add_chunk_chaining_value(std::array<std::uint32_t, 8> value, std::uint64_t total_chunks);

    ChunkState chunk_{0};
    std::array<std::array<std::uint32_t, 8>, 54> cv_stack_{};
    std::size_t cv_stack_len_ = 0;
};

}  // namespace auru::pm::detail

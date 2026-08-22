#include "blake3.hpp"

#include <algorithm>
#include <cstring>

namespace auru::pm::detail {
namespace {

constexpr std::uint32_t kChunkStart = 1;
constexpr std::uint32_t kChunkEnd = 2;
constexpr std::uint32_t kParent = 4;
constexpr std::uint32_t kRoot = 8;

constexpr std::array<std::uint32_t, 8> kIv = {
    0x6a09e667u, 0xbb67ae85u, 0x3c6ef372u, 0xa54ff53au,
    0x510e527fu, 0x9b05688cu, 0x1f83d9abu, 0x5be0cd19u,
};

constexpr std::array<std::size_t, 16> kMsgPermutation = {
    2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8,
};

std::uint32_t rotate_right(std::uint32_t value, unsigned bits) {
    return (value >> bits) | (value << (32u - bits));
}

void g(std::array<std::uint32_t, 16>& state, std::size_t a, std::size_t b, std::size_t c,
       std::size_t d, std::uint32_t mx, std::uint32_t my) {
    state[a] = state[a] + state[b] + mx;
    state[d] = rotate_right(state[d] ^ state[a], 16);
    state[c] = state[c] + state[d];
    state[b] = rotate_right(state[b] ^ state[c], 12);
    state[a] = state[a] + state[b] + my;
    state[d] = rotate_right(state[d] ^ state[a], 8);
    state[c] = state[c] + state[d];
    state[b] = rotate_right(state[b] ^ state[c], 7);
}

void round(std::array<std::uint32_t, 16>& state, const std::array<std::uint32_t, 16>& m) {
    // Columns.
    g(state, 0, 4, 8, 12, m[0], m[1]);
    g(state, 1, 5, 9, 13, m[2], m[3]);
    g(state, 2, 6, 10, 14, m[4], m[5]);
    g(state, 3, 7, 11, 15, m[6], m[7]);
    // Diagonals.
    g(state, 0, 5, 10, 15, m[8], m[9]);
    g(state, 1, 6, 11, 12, m[10], m[11]);
    g(state, 2, 7, 8, 13, m[12], m[13]);
    g(state, 3, 4, 9, 14, m[14], m[15]);
}

void permute(std::array<std::uint32_t, 16>& m) {
    std::array<std::uint32_t, 16> permuted{};
    for (std::size_t index = 0; index < 16; ++index) {
        permuted[index] = m[kMsgPermutation[index]];
    }
    m = permuted;
}

std::array<std::uint32_t, 16> compress(const std::array<std::uint32_t, 8>& chaining_value,
                                       const std::array<std::uint32_t, 16>& block_words,
                                       std::uint64_t counter, std::uint32_t block_len,
                                       std::uint32_t flags) {
    std::array<std::uint32_t, 16> state = {
        chaining_value[0], chaining_value[1], chaining_value[2], chaining_value[3],
        chaining_value[4], chaining_value[5], chaining_value[6], chaining_value[7],
        kIv[0], kIv[1], kIv[2], kIv[3],
        static_cast<std::uint32_t>(counter),
        static_cast<std::uint32_t>(counter >> 32),
        block_len, flags,
    };

    std::array<std::uint32_t, 16> block = block_words;
    for (std::size_t index = 0; index < 7; ++index) {
        round(state, block);
        if (index < 6) {
            permute(block);
        }
    }

    for (std::size_t index = 0; index < 8; ++index) {
        state[index] ^= state[index + 8];
        state[index + 8] ^= chaining_value[index];
    }
    return state;
}

std::array<std::uint32_t, 16> words_from_block(const std::array<std::uint8_t, 64>& block) {
    std::array<std::uint32_t, 16> words{};
    for (std::size_t index = 0; index < 16; ++index) {
        const std::size_t offset = index * 4;
        words[index] = static_cast<std::uint32_t>(block[offset])
                       | (static_cast<std::uint32_t>(block[offset + 1]) << 8)
                       | (static_cast<std::uint32_t>(block[offset + 2]) << 16)
                       | (static_cast<std::uint32_t>(block[offset + 3]) << 24);
    }
    return words;
}

}  // namespace

std::array<std::uint32_t, 8> Blake3::Output::chaining_value() const {
    const auto state = compress(input_chaining_value, block_words, counter, block_len, flags);
    std::array<std::uint32_t, 8> value{};
    for (std::size_t index = 0; index < 8; ++index) {
        value[index] = state[index];
    }
    return value;
}

std::array<std::uint8_t, 32> Blake3::Output::root_bytes() const {
    const auto words = compress(input_chaining_value, block_words, 0, block_len, flags | kRoot);
    std::array<std::uint8_t, 32> out{};
    for (std::size_t index = 0; index < 8; ++index) {
        const std::uint32_t word = words[index];
        out[index * 4] = static_cast<std::uint8_t>(word);
        out[index * 4 + 1] = static_cast<std::uint8_t>(word >> 8);
        out[index * 4 + 2] = static_cast<std::uint8_t>(word >> 16);
        out[index * 4 + 3] = static_cast<std::uint8_t>(word >> 24);
    }
    return out;
}

Blake3::ChunkState::ChunkState(std::uint64_t counter)
    : chaining_value(kIv), chunk_counter(counter) {}

std::size_t Blake3::ChunkState::length() const noexcept {
    return kBlockLen * blocks_compressed + block_len;
}

std::uint32_t Blake3::ChunkState::start_flag() const noexcept {
    return blocks_compressed == 0 ? kChunkStart : 0u;
}

void Blake3::ChunkState::update(const std::uint8_t* input, std::size_t length) {
    while (length > 0) {
        if (block_len == kBlockLen) {
            // A full block is only compressed once more input arrives: the
            // final block of a chunk carries CHUNK_END, and whether this is the
            // final one is not yet known.
            const auto state =
                compress(chaining_value, words_from_block(block), chunk_counter,
                         static_cast<std::uint32_t>(kBlockLen), start_flag());
            for (std::size_t index = 0; index < 8; ++index) {
                chaining_value[index] = state[index];
            }
            ++blocks_compressed;
            block.fill(0);
            block_len = 0;
        }

        const std::size_t take = std::min(kBlockLen - block_len, length);
        std::memcpy(block.data() + block_len, input, take);
        block_len += static_cast<std::uint32_t>(take);
        input += take;
        length -= take;
    }
}

Blake3::Output Blake3::ChunkState::output() const {
    Output out;
    out.input_chaining_value = chaining_value;
    out.block_words = words_from_block(block);
    out.counter = chunk_counter;
    out.block_len = block_len;
    out.flags = start_flag() | kChunkEnd;
    return out;
}

Blake3::Blake3() = default;

void Blake3::add_chunk_chaining_value(std::array<std::uint32_t, 8> value,
                                      std::uint64_t total_chunks) {
    // Every trailing zero bit of the chunk count is one subtree that just became
    // complete, so this merges exactly that many — which is what keeps the stack
    // shallow enough for its fixed bound.
    while ((total_chunks & 1u) == 0) {
        Output parent;
        parent.input_chaining_value = kIv;
        for (std::size_t index = 0; index < 8; ++index) {
            parent.block_words[index] = cv_stack_[cv_stack_len_ - 1][index];
            parent.block_words[index + 8] = value[index];
        }
        parent.counter = 0;
        parent.block_len = static_cast<std::uint32_t>(kBlockLen);
        parent.flags = kParent;

        --cv_stack_len_;
        value = parent.chaining_value();
        total_chunks >>= 1;
    }
    cv_stack_[cv_stack_len_++] = value;
}

void Blake3::update(const void* data, std::size_t length) {
    const auto* input = static_cast<const std::uint8_t*>(data);
    while (length > 0) {
        if (chunk_.length() == kChunkLen) {
            const std::uint64_t total_chunks = chunk_.chunk_counter + 1;
            add_chunk_chaining_value(chunk_.output().chaining_value(), total_chunks);
            chunk_ = ChunkState(total_chunks);
        }

        const std::size_t take = std::min(kChunkLen - chunk_.length(), length);
        chunk_.update(input, take);
        input += take;
        length -= take;
    }
}

std::array<std::uint8_t, 32> Blake3::finish() const {
    Output output = chunk_.output();
    for (std::size_t index = cv_stack_len_; index > 0; --index) {
        Output parent;
        parent.input_chaining_value = kIv;
        const auto right = output.chaining_value();
        for (std::size_t word = 0; word < 8; ++word) {
            parent.block_words[word] = cv_stack_[index - 1][word];
            parent.block_words[word + 8] = right[word];
        }
        parent.counter = 0;
        parent.block_len = static_cast<std::uint32_t>(kBlockLen);
        parent.flags = kParent;
        output = parent;
    }
    return output.root_bytes();
}

std::array<std::uint8_t, 32> Blake3::hash(const void* data, std::size_t length) {
    Blake3 hasher;
    hasher.update(data, length);
    return hasher.finish();
}

}  // namespace auru::pm::detail

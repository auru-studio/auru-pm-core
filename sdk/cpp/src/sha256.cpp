#include "sha256.hpp"

#include <cstring>
#include <vector>

namespace auru::pm::detail {
namespace {

constexpr std::array<std::uint32_t, 64> kRoundConstants = {
    0x428a2f98u, 0x71374491u, 0xb5c0fbcfu, 0xe9b5dba5u, 0x3956c25bu, 0x59f111f1u, 0x923f82a4u,
    0xab1c5ed5u, 0xd807aa98u, 0x12835b01u, 0x243185beu, 0x550c7dc3u, 0x72be5d74u, 0x80deb1feu,
    0x9bdc06a7u, 0xc19bf174u, 0xe49b69c1u, 0xefbe4786u, 0x0fc19dc6u, 0x240ca1ccu, 0x2de92c6fu,
    0x4a7484aau, 0x5cb0a9dcu, 0x76f988dau, 0x983e5152u, 0xa831c66du, 0xb00327c8u, 0xbf597fc7u,
    0xc6e00bf3u, 0xd5a79147u, 0x06ca6351u, 0x14292967u, 0x27b70a85u, 0x2e1b2138u, 0x4d2c6dfcu,
    0x53380d13u, 0x650a7354u, 0x766a0abbu, 0x81c2c92eu, 0x92722c85u, 0xa2bfe8a1u, 0xa81a664bu,
    0xc24b8b70u, 0xc76c51a3u, 0xd192e819u, 0xd6990624u, 0xf40e3585u, 0x106aa070u, 0x19a4c116u,
    0x1e376c08u, 0x2748774cu, 0x34b0bcb5u, 0x391c0cb3u, 0x4ed8aa4au, 0x5b9cca4fu, 0x682e6ff3u,
    0x748f82eeu, 0x78a5636fu, 0x84c87814u, 0x8cc70208u, 0x90befffau, 0xa4506cebu, 0xbef9a3f7u,
    0xc67178f2u,
};

std::uint32_t rotate_right(std::uint32_t value, unsigned bits) {
    return (value >> bits) | (value << (32u - bits));
}

}  // namespace

std::array<std::uint8_t, 32> sha256(const void* data, std::size_t length) {
    std::array<std::uint32_t, 8> state = {
        0x6a09e667u, 0xbb67ae85u, 0x3c6ef372u, 0xa54ff53au,
        0x510e527fu, 0x9b05688cu, 0x1f83d9abu, 0x5be0cd19u,
    };

    // Pad to a multiple of 64 bytes: a 0x80 byte, zeros, then the bit length.
    const auto* input = static_cast<const std::uint8_t*>(data);
    const std::uint64_t bit_length = static_cast<std::uint64_t>(length) * 8u;
    const std::size_t padded_length = ((length + 8) / 64 + 1) * 64;

    std::vector<std::uint8_t> message(padded_length, 0);
    std::memcpy(message.data(), input, length);
    message[length] = 0x80u;
    for (std::size_t index = 0; index < 8; ++index) {
        message[padded_length - 1 - index] = static_cast<std::uint8_t>(bit_length >> (8 * index));
    }

    std::array<std::uint32_t, 64> schedule{};
    for (std::size_t block = 0; block < padded_length; block += 64) {
        for (std::size_t index = 0; index < 16; ++index) {
            const std::size_t offset = block + index * 4;
            schedule[index] = (static_cast<std::uint32_t>(message[offset]) << 24)
                              | (static_cast<std::uint32_t>(message[offset + 1]) << 16)
                              | (static_cast<std::uint32_t>(message[offset + 2]) << 8)
                              | static_cast<std::uint32_t>(message[offset + 3]);
        }
        for (std::size_t index = 16; index < 64; ++index) {
            const std::uint32_t s0 = rotate_right(schedule[index - 15], 7)
                                     ^ rotate_right(schedule[index - 15], 18)
                                     ^ (schedule[index - 15] >> 3);
            const std::uint32_t s1 = rotate_right(schedule[index - 2], 17)
                                     ^ rotate_right(schedule[index - 2], 19)
                                     ^ (schedule[index - 2] >> 10);
            schedule[index] = schedule[index - 16] + s0 + schedule[index - 7] + s1;
        }

        std::array<std::uint32_t, 8> working = state;
        for (std::size_t index = 0; index < 64; ++index) {
            const std::uint32_t s1 = rotate_right(working[4], 6) ^ rotate_right(working[4], 11)
                                     ^ rotate_right(working[4], 25);
            const std::uint32_t choose =
                (working[4] & working[5]) ^ (~working[4] & working[6]);
            const std::uint32_t temp1 =
                working[7] + s1 + choose + kRoundConstants[index] + schedule[index];
            const std::uint32_t s0 = rotate_right(working[0], 2) ^ rotate_right(working[0], 13)
                                     ^ rotate_right(working[0], 22);
            const std::uint32_t majority =
                (working[0] & working[1]) ^ (working[0] & working[2]) ^ (working[1] & working[2]);
            const std::uint32_t temp2 = s0 + majority;

            working[7] = working[6];
            working[6] = working[5];
            working[5] = working[4];
            working[4] = working[3] + temp1;
            working[3] = working[2];
            working[2] = working[1];
            working[1] = working[0];
            working[0] = temp1 + temp2;
        }
        for (std::size_t index = 0; index < 8; ++index) {
            state[index] += working[index];
        }
    }

    std::array<std::uint8_t, 32> digest{};
    for (std::size_t index = 0; index < 8; ++index) {
        digest[index * 4] = static_cast<std::uint8_t>(state[index] >> 24);
        digest[index * 4 + 1] = static_cast<std::uint8_t>(state[index] >> 16);
        digest[index * 4 + 2] = static_cast<std::uint8_t>(state[index] >> 8);
        digest[index * 4 + 3] = static_cast<std::uint8_t>(state[index]);
    }
    return digest;
}

}  // namespace auru::pm::detail

#include <fstream>
#include <regex>
#include <sstream>
#include <string>
#include <vector>

#include "auru/pm/content_hash.hpp"
#include "harness.hpp"

using auru::pm::ContentHash;

namespace {

struct Vector {
    std::size_t length;
    std::string hash;
};

/// The vector file is read with a regex rather than through this library's own
/// JSON parser, so a failure here means the hash is wrong and nothing else. Two
/// components that can only be tested together can only be debugged together.
std::vector<Vector> vectors() {
    std::ifstream file(std::string(AURU_REPO_ROOT) + "/spec/vectors/content-hash.json");
    std::stringstream buffer;
    buffer << file.rdbuf();
    const std::string text = buffer.str();

    const std::regex pattern(
        R"~(\{\s*"hash":\s*"(blake3:[0-9a-f]{64})",\s*"length":\s*(\d+)\s*\})~");
    std::vector<Vector> found;
    for (auto match = std::sregex_iterator(text.begin(), text.end(), pattern);
         match != std::sregex_iterator(); ++match) {
        found.push_back({static_cast<std::size_t>(std::stoul((*match)[2].str())), (*match)[1].str()});
    }
    return found;
}

/// Byte `i` is `i % 251`, the pattern BLAKE3's own test suite uses.
std::vector<std::uint8_t> input(std::size_t length) {
    std::vector<std::uint8_t> bytes(length);
    for (std::size_t index = 0; index < length; ++index) {
        bytes[index] = static_cast<std::uint8_t>(index % 251);
    }
    return bytes;
}

}  // namespace

TEST("reproduces every published vector") {
    const auto published = vectors();
    CHECK(published.size() >= 30);
    for (const auto& vector : published) {
        const auto bytes = input(vector.length);
        const auto actual = ContentHash::of(bytes.data(), bytes.size()).to_string();
        if (actual != vector.hash) {
            std::ostringstream out;
            out << "input of " << vector.length << " bytes\n           expected " << vector.hash
                << "\n           got      " << actual;
            harness::fail(__FILE__, __LINE__, out.str());
        }
    }
}

TEST("covers the chunk and tree boundaries") {
    // A port that only handles one chunk would pass a careless vector list.
    const auto published = vectors();
    for (std::size_t boundary : {std::size_t{64}, std::size_t{1024}, std::size_t{1025},
                                 std::size_t{2048}, std::size_t{4096}}) {
        bool present = false;
        for (const auto& vector : published) {
            present = present || vector.length == boundary;
        }
        CHECK(present);
    }
    bool large = false;
    for (const auto& vector : published) {
        large = large || vector.length > 65536;
    }
    CHECK(large);
}

TEST("hashes the empty input to the known value") {
    CHECK_EQ(ContentHash::of("").to_string(),
             std::string("blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"));
}

TEST("parses and renders the canonical form") {
    const std::string text =
        "blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";
    const auto parsed = ContentHash::parse(text);
    CHECK(parsed.has_value());
    CHECK_EQ(parsed.value().to_string(), text);
    CHECK(parsed.value() == ContentHash::of(""));
}

TEST("rejects anything that is not the canonical form") {
    CHECK(!ContentHash::parse("deadbeef").has_value());
    CHECK(!ContentHash::parse("sha256:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262")
               .has_value());
    // Uppercase would compare unequal as a string to the same hash in lowercase.
    CHECK(!ContentHash::parse("blake3:AF1349B9F5F9A1A6A0404DEA36DCC9499BCB25C9ADC112B7CC9A93CAE41F3262")
               .has_value());
}

TEST("verifies bytes against their own name") {
    const std::string audio = "kick.wav";
    const auto hash = ContentHash::of(audio);
    CHECK(hash.matches(audio.data(), audio.size()));
    const std::string other = "snare.wav";
    CHECK(!hash.matches(other.data(), other.size()));
}

TEST_MAIN("blake3")

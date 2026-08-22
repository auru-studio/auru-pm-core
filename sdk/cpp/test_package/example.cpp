// The package is only useful if it links, its headers are complete, and the
// canonical rule survived packaging. This checks all three offline: it derives
// a commit id and compares it against a value from
// `spec/vectors/commit-encoding.json`, so a build that silently produced a
// different encoding fails here rather than at a provider.

#include <cstdlib>
#include <iostream>
#include <string>

#include "auru/pm/commit.hpp"
#include "auru/pm/content_hash.hpp"
#include "auru/pm/types.hpp"

int main() {
    using namespace auru::pm;

    if (ContentHash::of("").to_string()
        != "blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262") {
        std::cerr << "BLAKE3 does not match its known value\n";
        return EXIT_FAILURE;
    }

    AuthorIdentity author;
    author.display_name = "Test User";
    author.provider_user_id = "user-1";
    author.provider_id = "local-folder";

    Commit::Builder builder;
    auto commit = builder.tree(TreeRef{ContentHash::of("snapshot"), ContentHash::of("samples")})
                      .author(author)
                      .timestamp(1700000000)
                      .message("first take")
                      .auru_version("0.1.0")
                      .format_version(8)
                      .build();

    if (!commit) {
        std::cerr << "build: " << commit.error().to_string() << "\n";
        return EXIT_FAILURE;
    }

    // The "baseline" case in spec/vectors/commit-encoding.json.
    const std::string expected =
        "blake3:8189ac12713762dcb92c060a1db950b532c06dc10ac9a83d05344a67f25906e7";
    if (commit.value().id().to_string() != expected) {
        std::cerr << "commit id\n  expected " << expected << "\n  got      "
                  << commit.value().id().to_string() << "\n";
        return EXIT_FAILURE;
    }

    std::cout << "protocol: " << kProtocolVersion << "\n";
    std::cout << "commit:   " << commit.value().id().to_string() << "\n";
    std::cout << "packaged library reproduces the published vector\n";
    return EXIT_SUCCESS;
}

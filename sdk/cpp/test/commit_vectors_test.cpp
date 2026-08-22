#include <fstream>
#include <sstream>
#include <string>

#include "auru/pm/commit.hpp"
#include "auru/pm/json.hpp"
#include "harness.hpp"

using auru::pm::Commit;
using auru::pm::ContentHash;
using auru::pm::Json;

namespace {

/// The conformance gate.
///
/// A commit whose id this library cannot reproduce is rejected by every
/// provider, so these are not stylistic assertions — they are the contract. The
/// cases come from `spec/vectors/commit-encoding.json`, generated from the Rust
/// implementation, and the TypeScript and Java SDKs are checked against the
/// same file.
Json vectors() {
    std::ifstream file(std::string(AURU_REPO_ROOT) + "/spec/vectors/commit-encoding.json");
    std::stringstream buffer;
    buffer << file.rdbuf();
    auto parsed = Json::parse(buffer.str());
    if (!parsed) {
        harness::fail(__FILE__, __LINE__, "cannot read the vectors: " + parsed.error().message);
    }
    return parsed.value();
}

const std::vector<Json>& cases(const Json& document) {
    const Json* found = document.find("cases");
    if (found == nullptr) {
        harness::fail(__FILE__, __LINE__, "the vector file has no cases");
    }
    return found->elements();
}

}  // namespace

TEST("publishes the rule it is checked against") {
    const auto rule = vectors().string_at("rule");
    CHECK(rule.has_value());
    CHECK(rule.value().find("RFC 8785") != std::string::npos);
}

TEST("reproduces the canonical bytes for every case") {
    const Json document = vectors();
    const auto& all = cases(document);
    CHECK(all.size() >= 10);

    for (const Json& testCase : all) {
        const std::string name = testCase.string_at("name").value_or("?");
        const Json* commit_json = testCase.find("commit");
        CHECK(commit_json != nullptr);

        auto commit = Commit::from_json(*commit_json);
        if (!commit) {
            harness::fail(__FILE__, __LINE__, name + ": " + commit.error().message);
        }

        auto canonical = commit.value().canonical_encoding();
        if (!canonical) {
            harness::fail(__FILE__, __LINE__, name + ": " + canonical.error().message);
        }

        const std::string expected = testCase.string_at("canonical").value_or("");
        if (canonical.value() != expected) {
            std::ostringstream out;
            out << name << "\n           expected " << expected << "\n           got      "
                << canonical.value();
            harness::fail(__FILE__, __LINE__, out.str());
        }
    }
}

TEST("derives the recorded id for every case") {
    const Json document = vectors();
    for (const Json& testCase : cases(document)) {
        const std::string name = testCase.string_at("name").value_or("?");
        auto commit = Commit::from_json(*testCase.find("commit"));
        CHECK(commit.has_value());

        const std::string expected = testCase.string_at("id").value_or("");
        if (commit.value().id().to_string() != expected) {
            harness::fail(__FILE__, __LINE__,
                          name + "\n           expected " + expected + "\n           got      "
                              + commit.value().id().to_string());
        }
        CHECK(commit.value().verify_id());
    }
}

TEST("a builder derives the same id, ignoring any id supplied") {
    const Json document = vectors();
    const Json& first = cases(document).front();
    auto original = Commit::from_json(*first.find("commit"));
    CHECK(original.has_value());
    const Commit& commit = original.value();

    Commit::Builder builder;
    builder.parents(commit.parents())
        .tree(commit.tree())
        .author(commit.author())
        .timestamp(commit.timestamp())
        .message(commit.message())
        .description(commit.description())
        .auru_version(commit.auru_version())
        .format_version(commit.format_version());
    if (commit.metadata()) {
        builder.metadata(*commit.metadata());
    }

    auto rebuilt = builder.build();
    CHECK(rebuilt.has_value());
    // Identity is a function of content, not of itself.
    CHECK(rebuilt.value().id() == commit.id());
}

TEST("changes the id when any content changes") {
    const Json document = vectors();
    auto original = Commit::from_json(*cases(document).front().find("commit"));
    const Commit& commit = original.value();

    Commit::Builder builder;
    auto edited = builder.parents(commit.parents())
                      .tree(commit.tree())
                      .author(commit.author())
                      .timestamp(commit.timestamp())
                      .message("a different message")
                      .description(commit.description())
                      .auru_version(commit.auru_version())
                      .format_version(commit.format_version())
                      .build();
    CHECK(edited.has_value());
    CHECK(!(edited.value().id() == commit.id()));
}

TEST("the canonical encoding carries no id member") {
    const Json document = vectors();
    for (const Json& testCase : cases(document)) {
        auto commit = Commit::from_json(*testCase.find("commit"));
        CHECK(commit.has_value());
        CHECK(commit.value().canonical_encoding().value().find("\"id\"") == std::string::npos);
    }
}

TEST("to_json round trips through from_json") {
    // `canonical_encoding` and `to_json` are different code paths: the first is
    // what a hash is derived from, the second is what goes on the wire. A test
    // that only exercises one leaves the other free to be wrong.
    const Json document = vectors();
    for (const Json& testCase : cases(document)) {
        const std::string name = testCase.string_at("name").value_or("?");
        auto original = Commit::from_json(*testCase.find("commit"));
        CHECK(original.has_value());

        const Json wire = original.value().to_json();
        auto reparsed = Commit::from_json(wire);
        if (!reparsed) {
            harness::fail(__FILE__, __LINE__, name + ": " + reparsed.error().message);
        }
        CHECK(reparsed.value().id() == original.value().id());
        CHECK(reparsed.value().verify_id());
        CHECK_EQ(reparsed.value().message(), original.value().message());
        CHECK_EQ(reparsed.value().parents().size(), original.value().parents().size());
        CHECK_EQ(reparsed.value().metadata().has_value(),
                 original.value().metadata().has_value());
    }
}

TEST("canonicalizing is idempotent") {
    // Re-canonicalizing already-canonical bytes must change nothing. This
    // separates a parser fault from a writer fault: a parser that loses
    // information shows up here rather than only in the byte comparison.
    const Json document = vectors();
    for (const Json& testCase : cases(document)) {
        const std::string canonical = testCase.string_at("canonical").value_or("");
        auto reparsed = Json::parse(canonical);
        CHECK(reparsed.has_value());
        CHECK_EQ(reparsed.value().to_canonical_json().value(), canonical);
    }
}

TEST("refuses an integer outside what RFC 8785 can represent") {
    // Beyond 2^53 two conformant implementations derive different ids, so
    // producing bytes at all here would be worse than failing.
    Json json = Json::object({});
    json.set("timestamp", Json::integer(9007199254740993LL));
    auto encoded = json.to_canonical_json();
    CHECK(!encoded.has_value());
    CHECK(encoded.error().message.find("2^53") != std::string::npos);
}

TEST("refuses to canonicalize a fractional number") {
    auto json = Json::parse("{\"tempo\":128.5}");
    auto encoded = json.value().to_canonical_json();
    CHECK(!encoded.has_value());
    CHECK(encoded.error().message.find("non-integral") != std::string::npos);
}

TEST_MAIN("commit vectors")

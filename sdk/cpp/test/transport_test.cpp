#include <fstream>
#include <memory>
#include <sstream>
#include <string>
#include <vector>

#include "auru/pm/client.hpp"
#include "harness.hpp"
#include "socket_transport.hpp"
#include "test_provider.hpp"

using auru::pm::AuruClient;
using auru::pm::AuthorIdentity;
using auru::pm::Commit;
using auru::pm::CommitSummary;
using auru::pm::ContentHash;
using auru::pm::ErrorCode;
using auru::pm::ProjectClient;
using auru::pm::ProjectProfile;
using auru::pm::RetentionRequest;
using auru::pm::RetentionRule;
using auru::pm::TreeRef;

namespace {

TestProvider& provider() {
    static TestProvider running;
    return running;
}

AuruClient connect() {
    auto client = AuruClient::connect(
        {provider().endpoint(), std::make_shared<SocketTransport>(), std::nullopt});
    if (!client) {
        harness::fail(__FILE__, __LINE__, "cannot connect: " + client.error().message);
    }
    return client.value();
}

std::vector<std::uint8_t> fixture() {
    std::ifstream file(
        std::string(AURU_REPO_ROOT)
            + "/crates/auru-pm-kernel/tests/fixtures/interchange/oracle-midi.dawproject",
        std::ios::binary);
    return std::vector<std::uint8_t>((std::istreambuf_iterator<char>(file)),
                                     std::istreambuf_iterator<char>());
}

std::vector<std::uint8_t> bytes_of(const std::string& text) {
    return std::vector<std::uint8_t>(text.begin(), text.end());
}

/// Create a handle. A provider only knows about one once its profile exists.
ProjectClient create_project(const AuruClient& client, const std::string& handle,
                             const std::string& name) {
    auto project = client.project(handle);
    if (!project) {
        harness::fail(__FILE__, __LINE__, project.error().message);
    }
    ProjectProfile profile;
    profile.display_name = name;
    profile.format = "dawproject";
    auto stored = project.value().put_profile(profile);
    if (!stored) {
        harness::fail(__FILE__, __LINE__, "put_profile: " + stored.error().message);
    }
    return project.value();
}

/// Build, upload and record a version the way a real client would.
Commit publish(const AuruClient& client, const ProjectClient& project, const std::string& message,
               const std::vector<ContentHash>& parents) {
    auto identity = client.me();
    if (!identity) {
        harness::fail(__FILE__, __LINE__, "me: " + identity.error().message);
    }

    auto snapshot = project.put_blob(fixture());
    auto samples = project.put_blob(bytes_of("{\"entries\":[]}"));
    if (!snapshot || !samples) {
        harness::fail(__FILE__, __LINE__, "blob upload failed");
    }

    Commit::Builder builder;
    auto commit = builder.parents(parents)
                      .tree(TreeRef{snapshot.value(), samples.value()})
                      .author(identity.value().as_author())
                      .timestamp(1700000000)
                      .message(message)
                      .auru_version("0.1.0")
                      .format_version(1)
                      .build();
    if (!commit) {
        harness::fail(__FILE__, __LINE__, "build: " + commit.error().message);
    }

    auto stored = project.put_commit(commit.value());
    if (!stored) {
        harness::fail(__FILE__, __LINE__, "put_commit: " + stored.error().message);
    }
    CHECK(stored.value() == commit.value().id());
    return commit.value();
}

}  // namespace

TEST("reports the provider's protocol and capabilities") {
    const AuruClient client = connect();
    CHECK_EQ(client.health().protocol, std::string("auru-pm-v1"));
    CHECK_EQ(client.health().provider_id.value_or(""), std::string("cpp-sdk-test"));
    CHECK(client.capabilities().project_listing);
}

TEST("accepts an endpoint with a trailing slash") {
    auto client = AuruClient::connect(
        {provider().endpoint() + "/", std::make_shared<SocketTransport>(), std::nullopt});
    CHECK(client.has_value());
    CHECK_EQ(client.value().endpoint(), provider().endpoint());
}

TEST("refuses an endpoint that is not http") {
    auto client = AuruClient::connect(
        {"ftp://pm.example.com", std::make_shared<SocketTransport>(), std::nullopt});
    CHECK(!client.has_value());
    CHECK(client.error().code == ErrorCode::BadRequest);
}

TEST("refuses to connect without a transport") {
    // C++ has no standard HTTP client, so there is nothing sensible to default
    // to — saying so plainly beats a null dereference.
    auto client = AuruClient::connect({provider().endpoint(), nullptr, std::nullopt});
    CHECK(!client.has_value());
    CHECK(client.error().message.find("Transport is required") != std::string::npos);
}

TEST("publishes a version and reads it back") {
    const AuruClient client = connect();
    const ProjectClient project = create_project(client, "life-cycle", "Life Cycle");

    auto empty = project.head();
    CHECK(empty.has_value());
    CHECK(!empty.value().has_value());

    const Commit commit = publish(client, project, "first take", {});
    CHECK(project.advance_head(std::nullopt, commit.id()).has_value());

    auto head = project.head();
    CHECK(head.value().value() == commit.id());

    auto fetched = project.get_commit(commit.id());
    CHECK(fetched.has_value());
    CHECK_EQ(fetched.value().message(), std::string("first take"));
    // Round-tripping through the provider must not disturb identity.
    CHECK(fetched.value().verify_id());

    auto history = project.history();
    CHECK(history.has_value());
    CHECK_EQ(history.value().size(), std::size_t{1});
    CHECK_EQ(history.value().front().message, std::string("first take"));
}

TEST("stores a profile and lists the project") {
    const AuruClient client = connect();
    const ProjectClient project = create_project(client, "catalogued", "placeholder");
    const Commit commit = publish(client, project, "catalogued", {});
    CHECK(project.advance_head(std::nullopt, commit.id()).has_value());

    ProjectProfile profile;
    profile.display_name = "Night Drive";
    profile.format = "dawproject";
    profile.genre = "Drum & Bass, Jungle";
    profile.tags = {"wip"};
    CHECK(project.put_profile(profile).has_value());

    auto projects = client.list_projects();
    CHECK(projects.has_value());
    bool found = false;
    for (const auto& listed : projects.value()) {
        if (listed.handle == "catalogued") {
            found = true;
            CHECK_EQ(listed.profile.value().display_name, std::string("Night Drive"));
            CHECK_EQ(listed.profile.value().tags.size(), std::size_t{1});
        }
    }
    CHECK(found);
}

TEST("pages history newest first") {
    const AuruClient client = connect();
    const ProjectClient project = create_project(client, "paged", "Paged");

    std::optional<ContentHash> parent;
    ContentHash newest = ContentHash::of("");
    for (const char* message : {"one", "two", "three"}) {
        std::vector<ContentHash> parents;
        if (parent) {
            parents.push_back(*parent);
        }
        const Commit commit = publish(client, project, message, parents);
        CHECK(project.advance_head(parent, commit.id()).has_value());
        parent = commit.id();
        newest = commit.id();
    }

    auto all = project.history();
    CHECK_EQ(all.value().size(), std::size_t{3});
    CHECK_EQ(all.value()[0].message, std::string("three"));
    CHECK_EQ(all.value()[2].message, std::string("one"));

    auto limited = project.history({2, std::nullopt});
    CHECK_EQ(limited.value().size(), std::size_t{2});

    auto older = project.history({0, newest});
    CHECK_EQ(older.value().size(), std::size_t{2});
    CHECK_EQ(older.value()[0].message, std::string("two"));
}

TEST("reports the actual HEAD when the caller's is stale") {
    const AuruClient client = connect();
    const ProjectClient project = create_project(client, "racing", "Racing");

    const Commit first = publish(client, project, "first", {});
    CHECK(project.advance_head(std::nullopt, first.id()).has_value());
    const Commit second = publish(client, project, "second", {first.id()});
    CHECK(project.advance_head(first.id(), second.id()).has_value());

    // A client that still believes HEAD is `first` loses the race, and has to
    // learn what it lost to without another round trip.
    const Commit stale = publish(client, project, "stale", {first.id()});
    auto conflict = project.advance_head(first.id(), stale.id());
    CHECK(!conflict.has_value());
    CHECK(conflict.error().code == ErrorCode::HeadConflict);
    CHECK(conflict.error().current_head.has_value());
    CHECK_EQ(conflict.error().current_head.value(), second.id().to_string());
    CHECK(!conflict.error().retryable());
}

TEST("verifies a download against its own name") {
    const AuruClient client = connect();
    const ProjectClient project = create_project(client, "blobs", "Blobs");
    const auto audio = bytes_of("kick.wav pretending to be audio");
    const ContentHash hash = ContentHash::of(audio.data(), audio.size());

    auto absent = project.has_blobs({hash});
    CHECK_EQ(absent.value().size(), std::size_t{1});
    CHECK(!absent.value()[0]);

    auto stored = project.put_blob(audio);
    CHECK(stored.value() == hash);

    auto present = project.has_blobs({hash});
    CHECK(present.value()[0]);

    auto fetched = project.get_blob(hash);
    CHECK(fetched.has_value());
    CHECK(fetched.value() == audio);

    // Re-uploading is not an error.
    CHECK(project.put_blob(audio).has_value());
}

TEST("reads a project summary without fetching the snapshot") {
    const AuruClient client = connect();
    const ProjectClient project = create_project(client, "summarized", "Summarized");

    const auto summary = bytes_of(
        "{\"schema\":1,\"format\":\"dawproject\",\"dawproject\":{\"title\":\"Night Drive\"}}");
    auto summary_hash = project.put_blob(summary);
    auto snapshot = project.put_blob(fixture());
    auto samples = project.put_blob(bytes_of("{\"entries\":[]}"));

    Commit::Builder builder;
    auto commit = builder.tree(TreeRef{snapshot.value(), samples.value()})
                      .author(client.me().value().as_author())
                      .timestamp(1700000000)
                      .message("with a summary")
                      .auru_version("0.1.0")
                      .format_version(1)
                      .metadata(summary_hash.value())
                      .build();
    CHECK(project.put_commit(commit.value()).has_value());

    auto info = project.project_info(commit.value());
    CHECK(info.has_value());
    CHECK(info.value().has_value());
    CHECK_EQ(info.value()->format, std::string("dawproject"));
    CHECK_EQ(info.value()->text("title").value_or(""), std::string("Night Drive"));
    CHECK(summary.size() < fixture().size());
}

TEST("refuses a commit whose id does not match its content") {
    // The builder cannot produce one, so this goes through the wire type
    // directly — which is what a broken or hostile client would do.
    const AuruClient client = connect();
    const ProjectClient project = create_project(client, "tampered", "Tampered");
    auto snapshot = project.put_blob(fixture());
    auto samples = project.put_blob(bytes_of("{\"entries\":[]}"));

    auru::pm::Json forged = auru::pm::Json::object({});
    forged.set("id", auru::pm::Json::string("blake3:" + std::string(64, 'a')));
    forged.set("parents", auru::pm::Json::array({}));
    forged.set("tree", (TreeRef{snapshot.value(), samples.value()}).to_json());
    forged.set("author", client.me().value().as_author().to_json());
    forged.set("timestamp", auru::pm::Json::integer(1700000000));
    forged.set("message", auru::pm::Json::string("an id I did not compute"));
    forged.set("description", auru::pm::Json::string(""));
    forged.set("auru_version", auru::pm::Json::string("0.1.0"));
    forged.set("format_version", auru::pm::Json::integer(1));

    auto commit = Commit::from_json(forged);
    CHECK(commit.has_value());
    CHECK(!commit.value().verify_id());

    auto rejected = project.put_commit(commit.value());
    CHECK(!rejected.has_value());
    CHECK(rejected.error().code == ErrorCode::BadRequest);
}

TEST("reports a missing commit as not found") {
    const AuruClient client = connect();
    auto project = client.project("life-cycle");
    auto absent = ContentHash::parse("blake3:" + std::string(64, 'b'));
    auto missing = project.value().get_commit(absent.value());
    CHECK(!missing.has_value());
    CHECK(missing.error().code == ErrorCode::NotFound);
}

TEST("rejects an empty project handle before sending anything") {
    const AuruClient client = connect();
    CHECK(!client.project("").has_value());
}

TEST("prunes history when the provider supports it") {
    const AuruClient client = connect();
    CHECK(client.capabilities().history_retention);
    const ProjectClient project = create_project(client, "pruned", "Pruned");

    std::optional<ContentHash> parent;
    for (const char* message : {"one", "two", "three", "four"}) {
        std::vector<ContentHash> parents;
        if (parent) {
            parents.push_back(*parent);
        }
        const Commit commit = publish(client, project, message, parents);
        CHECK(project.advance_head(parent, commit.id()).has_value());
        parent = commit.id();
    }
    CHECK_EQ(project.history().value().size(), std::size_t{4});

    RetentionRequest request;
    request.rule = RetentionRule::latest(2);
    auto report = project.prune_history(request);
    CHECK(report.has_value());
    CHECK_EQ(report.value().versions_removed, std::int64_t{2});
    CHECK_EQ(project.history().value().size(), std::size_t{2});
}

TEST_MAIN("transport")

#include <memory>
#include <string>

#include "auru/pm/client.hpp"
#include "harness.hpp"
#include "stub_transport.hpp"

using auru::pm::AuruClient;
using auru::pm::ContentHash;
using auru::pm::ErrorCode;
using auru::pm::Response;

namespace {

/// A client whose non-health calls all answer `reply`.
std::pair<AuruClient, std::shared_ptr<StubTransport>> stub(Response reply) {
    auto transport = std::make_shared<StubTransport>([reply](const auru::pm::Request& request) {
        if (request.url.find("/v1/health") != std::string::npos) {
            return StubTransport::json(200, kStubHealth);
        }
        return reply;
    });
    auto client = AuruClient::connect({"http://pm.example.com", transport, std::nullopt});
    if (!client) {
        harness::fail(__FILE__, __LINE__, client.error().message);
    }
    return {client.value(), transport};
}

}  // namespace

TEST("keeps the code and message a provider sent") {
    auto [client, transport] =
        stub(StubTransport::json(403, "{\"code\":\"forbidden\",\"message\":\"not your project\"}"));
    auto failed = client.me();
    CHECK(!failed.has_value());
    CHECK(failed.error().code == ErrorCode::Forbidden);
    CHECK_EQ(failed.error().message, std::string("not your project"));
    CHECK_EQ(failed.error().status.value_or(0), 403);
    CHECK(!failed.error().retryable());
}

TEST("surfaces head conflict with the current head") {
    const std::string current = "blake3:" + std::string(64, 'c');
    auto [client, transport] = stub(
        StubTransport::json(409, "{\"code\":\"head_conflict\",\"current\":\"" + current + "\"}"));

    auto to = ContentHash::parse("blake3:" + std::string(64, 'd'));
    auto conflict = client.project("p").value().advance_head(std::nullopt, to.value());
    CHECK(!conflict.has_value());
    CHECK(conflict.error().code == ErrorCode::HeadConflict);
    CHECK_EQ(conflict.error().current_head.value_or(""), current);
}

TEST("reads a null current head on an empty project") {
    auto [client, transport] =
        stub(StubTransport::json(409, "{\"code\":\"head_conflict\",\"current\":null}"));
    auto to = ContentHash::parse("blake3:" + std::string(64, 'f'));
    auto conflict = client.project("p").value().advance_head(std::nullopt, to.value());
    CHECK(!conflict.has_value());
    CHECK(!conflict.error().current_head.has_value());
}

TEST("carries Retry-After on a rate limit and marks it retryable") {
    auto [client, transport] = stub(StubTransport::json(
        429, "{\"code\":\"rate_limited\",\"message\":\"slow down\"}", {{"Retry-After", "60"}}));
    auto failed = client.me();
    CHECK(failed.error().code == ErrorCode::RateLimited);
    CHECK_EQ(failed.error().retry_after_seconds.value_or(0), 60);
    CHECK(failed.error().retryable());
}

TEST("treats an unreachable identity provider as retryable, not as a bad token") {
    // Prompting someone to sign in again here would be wrong: the token was
    // never judged.
    auto [client, transport] = stub(StubTransport::json(
        503, "{\"code\":\"authentication_unavailable\",\"message\":\"idp down\"}"));
    auto failed = client.me();
    CHECK(failed.error().code == ErrorCode::AuthenticationUnavailable);
    CHECK(failed.error().retryable());
}

TEST("falls back to the status when a provider invents a code") {
    auto [client, transport] =
        stub(StubTransport::json(404, "{\"code\":\"teapot\",\"message\":\"unusual\"}"));
    auto failed = client.me();
    CHECK(failed.error().code == ErrorCode::NotFound);
    CHECK_EQ(failed.error().message, std::string("unusual"));
}

TEST("survives a proxy answering with HTML") {
    auto [client, transport] = stub(StubTransport::raw(502, "<html>502 Bad Gateway</html>"));
    auto failed = client.me();
    CHECK(failed.error().code == ErrorCode::Internal);
    CHECK(failed.error().retryable());
    CHECK(failed.error().message.find("502") != std::string::npos);
}

TEST("refuses a provider speaking a different wire version") {
    auto transport = std::make_shared<StubTransport>([](const auru::pm::Request&) {
        std::string health = kStubHealth;
        health.replace(health.find("auru-pm-v1"), std::string("auru-pm-v1").size(),
                       "auru-pm-v2");
        return StubTransport::json(200, health);
    });
    auto client = AuruClient::connect({"http://pm.example.com", transport, std::nullopt});
    CHECK(!client.has_value());
    CHECK(client.error().code == ErrorCode::Unsupported);
    CHECK(client.error().message.find("auru-pm-v2") != std::string::npos);
}

TEST("refuses an unadvertised capability before a round trip") {
    auto [client, transport] = stub(StubTransport::json(200, "{}"));
    CHECK(!client.capabilities().history_retention);

    auru::pm::RetentionRequest request;
    request.rule = auru::pm::RetentionRule::latest(10);
    auto refused = client.project("p").value().prune_history(request);
    CHECK(!refused.has_value());
    CHECK(refused.error().code == ErrorCode::Unsupported);
    // Nothing was sent: only the health request ever reached the stub.
    CHECK_EQ(transport->requests.size(), std::size_t{1});
}

TEST("sends the bearer token and never exposes it") {
    auto transport = std::make_shared<StubTransport>([](const auru::pm::Request& request) {
        if (request.url.find("/v1/health") != std::string::npos) {
            return StubTransport::json(200, kStubHealth);
        }
        return StubTransport::json(200, "{\"commit_id\":null}");
    });
    auto client =
        AuruClient::connect({"http://pm.example.com", transport, std::string("secret-token")});
    CHECK(client.value().authenticated());

    CHECK(client.value().project("p").value().head().has_value());
    bool sent = false;
    for (const auto& header : transport->requests.back().headers) {
        sent = sent || (header.first == "authorization" && header.second == "Bearer secret-token");
    }
    CHECK(sent);

    client.value().set_access_token(std::nullopt);
    CHECK(!client.value().authenticated());
    CHECK(client.value().project("p").value().head().has_value());
    for (const auto& header : transport->requests.back().headers) {
        CHECK(header.first != "authorization");
    }
}

TEST("verifies a downloaded blob and rejects tampered bytes") {
    auto [client, transport] = stub(StubTransport::raw(200, "not the bytes you asked for"));
    const std::string real = "the real bytes";
    const auto expected = ContentHash::of(real);

    auto tampered = client.project("p").value().get_blob(expected);
    CHECK(!tampered.has_value());
    CHECK(tampered.error().message.find("does not hash to its own name") != std::string::npos);
}

TEST("asking about no blobs is not a request") {
    auto [client, transport] = stub(StubTransport::json(500, "{\"code\":\"internal\"}"));
    auto none = client.project("p").value().has_blobs({});
    CHECK(none.has_value());
    CHECK(none.value().empty());
    CHECK_EQ(transport->requests.size(), std::size_t{1});
}

TEST_MAIN("error mapping")

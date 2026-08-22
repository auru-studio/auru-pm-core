#include <map>
#include <memory>
#include <string>

#include "auru/pm/oauth.hpp"
#include "auru/pm/types.hpp"
#include "harness.hpp"
#include "sha256.hpp"
#include "stub_transport.hpp"

using auru::pm::ErrorCode;
using auru::pm::OAuthClient;
using auru::pm::OAuthConfiguration;
using auru::pm::Json;
namespace oauth = auru::pm::oauth;

namespace {

const char* kIssuer = "https://identity.example.com";

std::string discovery_document() {
    return std::string("{\"issuer\":\"") + kIssuer
           + "\",\"authorization_endpoint\":\"" + kIssuer + "/authorize\","
           + "\"token_endpoint\":\"" + kIssuer + "/token\","
           + "\"code_challenge_methods_supported\":[\"S256\"]}";
}

const char* kTokenResponse =
    "{\"access_token\":\"an-access-token\",\"refresh_token\":\"a-refresh-token\","
    "\"token_type\":\"Bearer\",\"expires_in\":3600,\"scope\":\"openid\"}";

OAuthClient browser_client() {
    OAuthClient client;
    client.kind = OAuthClient::Kind::Browser;
    client.client_id = "dashboard";
    client.redirect_uri = "https://dashboard.example.com/oauth/callback";
    client.flows = {"authorization_code_pkce"};
    return client;
}

oauth::ServerMetadata metadata() {
    oauth::ServerMetadata found;
    found.issuer = kIssuer;
    found.authorization_endpoint = std::string(kIssuer) + "/authorize";
    found.token_endpoint = std::string(kIssuer) + "/token";
    found.code_challenge_methods_supported = {"S256"};
    return found;
}

std::map<std::string, std::string> query_of(const std::string& url) {
    std::map<std::string, std::string> parameters;
    const auto question = url.find('?');
    std::size_t start = question + 1;
    while (start < url.size()) {
        const auto end = url.find('&', start);
        const std::string pair =
            url.substr(start, end == std::string::npos ? std::string::npos : end - start);
        const auto equals = pair.find('=');
        if (equals != std::string::npos) {
            parameters[pair.substr(0, equals)] = pair.substr(equals + 1);
        }
        if (end == std::string::npos) {
            break;
        }
        start = end + 1;
    }
    return parameters;
}

}  // namespace

// ── SHA-256, which PKCE rests on ─────────────────────────────────────────────

TEST("sha256 matches the published FIPS vectors") {
    const auto hex = [](const std::array<std::uint8_t, 32>& digest) {
        static const char kHex[] = "0123456789abcdef";
        std::string text;
        for (std::uint8_t value : digest) {
            text.push_back(kHex[value >> 4]);
            text.push_back(kHex[value & 0x0F]);
        }
        return text;
    };

    CHECK_EQ(hex(auru::pm::detail::sha256("", 0)),
             std::string("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"));
    CHECK_EQ(hex(auru::pm::detail::sha256("abc", 3)),
             std::string("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
    const std::string long_input =
        "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
    CHECK_EQ(hex(auru::pm::detail::sha256(long_input.data(), long_input.size())),
             std::string("248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"));
    // Crosses the padding boundary, where a length-handling error hides.
    const std::string block(64, 'a');
    CHECK_EQ(hex(auru::pm::detail::sha256(block.data(), block.size())),
             std::string("ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"));
}

// ── Choosing a client ────────────────────────────────────────────────────────

TEST("finds each registered client kind") {
    auto json = Json::parse(
        std::string("{\"issuer\":\"") + kIssuer
        + "\",\"audience\":\"auru-pm\",\"required_scope\":\"openid\",\"clients\":["
        + "{\"kind\":\"native\",\"client_id\":\"desktop\","
        + "\"redirect_uri\":\"http://127.0.0.1:43827/oauth/callback\","
        + "\"flows\":[\"authorization_code_pkce\"]},"
        + "{\"kind\":\"browser\",\"client_id\":\"dashboard\","
        + "\"redirect_uri\":\"https://dashboard.example.com/cb\","
        + "\"flows\":[\"authorization_code_pkce\"]}]}");
    const auto configuration = OAuthConfiguration::from_json(json.value());

    CHECK_EQ(configuration.client(OAuthClient::Kind::Native).value().client_id,
             std::string("desktop"));
    CHECK_EQ(configuration.client(OAuthClient::Kind::Browser).value().client_id,
             std::string("dashboard"));
}

TEST("reads the pre-clients singular fields as the native client") {
    // What every provider written before `clients` existed sends.
    auto json = Json::parse(
        std::string("{\"issuer\":\"") + kIssuer
        + "\",\"audience\":\"auru-pm\",\"required_scope\":\"openid\",\"client_id\":\"desktop\","
        + "\"redirect_uri\":\"http://127.0.0.1:43827/oauth/callback\","
        + "\"flows\":[\"authorization_code_pkce\"]}");
    const auto configuration = OAuthConfiguration::from_json(json.value());

    CHECK_EQ(configuration.client(OAuthClient::Kind::Native).value().client_id,
             std::string("desktop"));
    CHECK(!configuration.client(OAuthClient::Kind::Browser).has_value());
}

// ── Discovery ────────────────────────────────────────────────────────────────

TEST("accepts a document whose issuer matches exactly") {
    StubTransport transport(
        [](const auru::pm::Request&) { return StubTransport::json(200, discovery_document()); });
    auto found = oauth::discover(kIssuer, transport);
    CHECK(found.has_value());
    CHECK_EQ(found.value().token_endpoint, std::string(kIssuer) + "/token");
}

TEST("refuses a document claiming a different issuer") {
    // A provider that could name someone else's issuer could send a user's
    // credentials there.
    StubTransport transport([](const auru::pm::Request&) {
        return StubTransport::json(
            200, "{\"issuer\":\"https://attacker.example.com\","
                 "\"authorization_endpoint\":\"https://attacker.example.com/a\","
                 "\"token_endpoint\":\"https://attacker.example.com/t\"}");
    });
    auto found = oauth::discover(kIssuer, transport);
    CHECK(!found.has_value());
    CHECK(found.error().message.find("claims issuer") != std::string::npos);
}

TEST("refuses a document missing the endpoints PKCE needs") {
    StubTransport transport([](const auru::pm::Request&) {
        return StubTransport::json(200, std::string("{\"issuer\":\"") + kIssuer + "\"}");
    });
    auto found = oauth::discover(kIssuer, transport);
    CHECK(!found.has_value());
    CHECK(found.error().message.find("omits the endpoints") != std::string::npos);
}

TEST("reports an issuer it cannot reach") {
    StubTransport transport(
        [](const auru::pm::Request&) { return StubTransport::raw(404, ""); });
    auto found = oauth::discover(kIssuer, transport);
    CHECK(!found.has_value());
    CHECK(found.error().message.find("cannot discover") != std::string::npos);
}

// ── Beginning authorization ──────────────────────────────────────────────────

TEST("builds an S256 challenge and a fresh state") {
    auto request = oauth::begin_authorization(metadata(), browser_client(), "openid");
    CHECK(request.has_value());

    const auto parameters = query_of(request.value().url);
    CHECK(request.value().url.rfind(std::string(kIssuer) + "/authorize?", 0) == 0);
    CHECK_EQ(parameters.at("response_type"), std::string("code"));
    CHECK_EQ(parameters.at("client_id"), std::string("dashboard"));
    CHECK_EQ(parameters.at("code_challenge_method"), std::string("S256"));
    CHECK_EQ(parameters.at("state"), request.value().state);

    // The challenge must actually be SHA-256 of the verifier, not a copy of it.
    CHECK(parameters.at("code_challenge") != request.value().code_verifier);
    CHECK(!request.value().code_verifier.empty());
}

TEST("gives every request its own verifier and state") {
    auto first = oauth::begin_authorization(metadata(), browser_client(), "openid");
    auto second = oauth::begin_authorization(metadata(), browser_client(), "openid");
    CHECK(first.value().code_verifier != second.value().code_verifier);
    CHECK(first.value().state != second.value().state);
}

TEST("refuses a provider that does not offer S256") {
    auto plain_only = metadata();
    plain_only.code_challenge_methods_supported = {"plain"};
    auto refused = oauth::begin_authorization(plain_only, browser_client(), "openid");
    CHECK(!refused.has_value());
    CHECK(refused.error().message.find("PKCE S256") != std::string::npos);
}

TEST("refuses a client not registered for the flow") {
    auto device_only = browser_client();
    device_only.flows = {"device_authorization"};
    auto refused = oauth::begin_authorization(metadata(), device_only, "openid");
    CHECK(!refused.has_value());
    CHECK(refused.error().message.find("authorization_code_pkce") != std::string::npos);
}

TEST("carries extra authorization parameters") {
    auto request =
        oauth::begin_authorization(metadata(), browser_client(), "openid", {{"prompt", "consent"}});
    CHECK_EQ(query_of(request.value().url).at("prompt"), std::string("consent"));
}

// ── The redirect ─────────────────────────────────────────────────────────────

TEST("refuses a redirect whose state does not match") {
    auto request = oauth::begin_authorization(metadata(), browser_client(), "openid");
    auto code = request.value().code_from("https://dashboard.example.com/cb?code=abc&state=other");
    CHECK(!code.has_value());
    CHECK(code.error().message.find("state does not match") != std::string::npos);
}

TEST("surfaces an error returned instead of a code") {
    auto request = oauth::begin_authorization(metadata(), browser_client(), "openid");
    auto code = request.value().code_from(
        "https://dashboard.example.com/cb?error=access_denied&error_description=user+said+no&state="
        + request.value().state);
    CHECK(!code.has_value());
    CHECK(code.error().message.find("user said no") != std::string::npos);
}

TEST("pulls the code out of a matching redirect") {
    auto request = oauth::begin_authorization(metadata(), browser_client(), "openid");
    auto code = request.value().code_from("https://dashboard.example.com/cb?code=the-code&state="
                                          + request.value().state);
    CHECK(code.has_value());
    CHECK_EQ(code.value(), std::string("the-code"));
}

// ── Completing authorization ─────────────────────────────────────────────────

TEST("discards the refresh token by default") {
    StubTransport transport(
        [](const auru::pm::Request&) { return StubTransport::json(200, kTokenResponse); });
    auto request = oauth::begin_authorization(metadata(), browser_client(), "openid");
    auto token = oauth::complete_authorization(metadata(), browser_client(), request.value(),
                                               "the-code", transport);
    CHECK(token.has_value());
    CHECK_EQ(token.value().token, std::string("an-access-token"));
    CHECK_EQ(token.value().expires_in_seconds.value_or(0), 3600);
    // There is no member that could return it, which is the point.
}

TEST("returns the refresh token only when asked for explicitly") {
    StubTransport transport(
        [](const auru::pm::Request&) { return StubTransport::json(200, kTokenResponse); });
    auto request = oauth::begin_authorization(metadata(), browser_client(), "openid");
    auto token = oauth::complete_authorization_with_refresh(metadata(), browser_client(),
                                                            request.value(), "the-code", transport);
    CHECK(token.has_value());
    CHECK_EQ(token.value().refresh_token.value_or(""), std::string("a-refresh-token"));
}

TEST("sends the verifier and no client secret") {
    StubTransport transport(
        [](const auru::pm::Request&) { return StubTransport::json(200, kTokenResponse); });
    auto request = oauth::begin_authorization(metadata(), browser_client(), "openid");
    oauth::complete_authorization(metadata(), browser_client(), request.value(), "the-code",
                                  transport);

    const auto& sent = transport.requests.back();
    const std::string body(sent.body.begin(), sent.body.end());
    CHECK(body.find("grant_type=authorization_code") != std::string::npos);
    CHECK(body.find("code_verifier=" + request.value().code_verifier) != std::string::npos);
    CHECK(body.find("client_id=dashboard") != std::string::npos);
    // A public client has no secret; sending one would mean it was shipped.
    CHECK(body.find("client_secret") == std::string::npos);
}

TEST("reports the provider's error description when exchange fails") {
    StubTransport transport([](const auru::pm::Request&) {
        return StubTransport::json(
            400, "{\"error\":\"invalid_grant\",\"error_description\":\"code already used\"}");
    });
    auto request = oauth::begin_authorization(metadata(), browser_client(), "openid");
    auto token = oauth::complete_authorization(metadata(), browser_client(), request.value(),
                                               "expired", transport);
    CHECK(!token.has_value());
    CHECK(token.error().message.find("code already used") != std::string::npos);
}

TEST("refuses a success response carrying no access token") {
    StubTransport transport([](const auru::pm::Request&) {
        return StubTransport::json(200, "{\"token_type\":\"Bearer\"}");
    });
    auto request = oauth::begin_authorization(metadata(), browser_client(), "openid");
    auto token = oauth::complete_authorization(metadata(), browser_client(), request.value(),
                                               "the-code", transport);
    CHECK(!token.has_value());
    CHECK(token.error().message.find("no access_token") != std::string::npos);
}

TEST_MAIN("oauth")

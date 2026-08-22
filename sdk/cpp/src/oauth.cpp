#include "auru/pm/oauth.hpp"

#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <utility>

#include "auru/pm/json.hpp"
#include "sha256.hpp"

namespace auru::pm::oauth {
namespace {

constexpr const char kBase64Url[] =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

std::string base64_url(const std::uint8_t* data, std::size_t length) {
    std::string encoded;
    encoded.reserve((length * 4 + 2) / 3);
    for (std::size_t index = 0; index < length; index += 3) {
        const std::uint32_t remaining = static_cast<std::uint32_t>(length - index);
        std::uint32_t chunk = static_cast<std::uint32_t>(data[index]) << 16;
        if (remaining > 1) {
            chunk |= static_cast<std::uint32_t>(data[index + 1]) << 8;
        }
        if (remaining > 2) {
            chunk |= static_cast<std::uint32_t>(data[index + 2]);
        }
        encoded.push_back(kBase64Url[(chunk >> 18) & 0x3Fu]);
        encoded.push_back(kBase64Url[(chunk >> 12) & 0x3Fu]);
        if (remaining > 1) {
            encoded.push_back(kBase64Url[(chunk >> 6) & 0x3Fu]);
        }
        if (remaining > 2) {
            encoded.push_back(kBase64Url[chunk & 0x3Fu]);
        }
    }
    return encoded;
}

/// Cryptographically secure bytes.
///
/// `std::random_device` is not required to be cryptographic — on some
/// implementations it is a plain PRNG with a fixed seed — so the platform CSPRNG
/// is read directly. A predictable PKCE verifier is a PKCE flow that protects
/// nothing.
Result<std::string> random_url_safe(std::size_t byte_length) {
    std::vector<std::uint8_t> bytes(byte_length);
#if defined(_WIN32)
// clang-format off
    if (BCryptGenRandom(nullptr, bytes.data(), static_cast<ULONG>(byte_length),
                        BCRYPT_USE_SYSTEM_PREFERRED_RNG)
        != 0) {
        return make_error<std::string>(ErrorCode::Internal,
                                       "the system random number generator is unavailable");
    }
#else
    std::ifstream urandom("/dev/urandom", std::ios::binary);
    if (!urandom) {
        return make_error<std::string>(ErrorCode::Internal, "cannot open /dev/urandom");
    }
    urandom.read(reinterpret_cast<char*>(bytes.data()),
                 static_cast<std::streamsize>(byte_length));
    if (!urandom) {
        return make_error<std::string>(ErrorCode::Internal, "cannot read /dev/urandom");
    }
#endif
    return Result<std::string>::ok(base64_url(bytes.data(), bytes.size()));
}

std::string url_encode(const std::string& value) {
    static const char kHex[] = "0123456789ABCDEF";
    std::string encoded;
    for (char raw : value) {
        const auto character = static_cast<unsigned char>(raw);
        const bool unreserved = (character >= 'A' && character <= 'Z')
                                || (character >= 'a' && character <= 'z')
                                || (character >= '0' && character <= '9') || character == '-'
                                || character == '_' || character == '.' || character == '~';
        if (unreserved) {
            encoded.push_back(raw);
        } else {
            encoded.push_back('%');
            encoded.push_back(kHex[character >> 4]);
            encoded.push_back(kHex[character & 0x0F]);
        }
    }
    return encoded;
}

std::string url_decode(const std::string& value) {
    std::string decoded;
    for (std::size_t index = 0; index < value.size(); ++index) {
        if (value[index] == '%' && index + 2 < value.size()) {
            decoded.push_back(static_cast<char>(
                std::strtol(value.substr(index + 1, 2).c_str(), nullptr, 16)));
            index += 2;
        } else if (value[index] == '+') {
            decoded.push_back(' ');
        } else {
            decoded.push_back(value[index]);
        }
    }
    return decoded;
}

std::string form(const std::vector<std::pair<std::string, std::string>>& parameters) {
    std::string encoded;
    for (const auto& parameter : parameters) {
        if (!encoded.empty()) {
            encoded.push_back('&');
        }
        encoded += url_encode(parameter.first) + "=" + url_encode(parameter.second);
    }
    return encoded;
}

std::map<std::string, std::string> parse_query(const std::string& url) {
    std::map<std::string, std::string> parameters;
    const auto question = url.find('?');
    if (question == std::string::npos) {
        return parameters;
    }
    const std::string query = url.substr(question + 1);
    std::size_t start = 0;
    while (start <= query.size()) {
        const auto end = query.find('&', start);
        const std::string pair =
            query.substr(start, end == std::string::npos ? std::string::npos : end - start);
        const auto equals = pair.find('=');
        if (equals != std::string::npos) {
            parameters[url_decode(pair.substr(0, equals))] = url_decode(pair.substr(equals + 1));
        }
        if (end == std::string::npos) {
            break;
        }
        start = end + 1;
    }
    return parameters;
}

std::vector<std::string> string_list(const Json* json) {
    std::vector<std::string> values;
    if (json == nullptr) {
        return values;
    }
    for (const Json& element : json->elements()) {
        if (auto text = element.as_text()) {
            values.push_back(*text);
        }
    }
    return values;
}

}  // namespace

Result<std::string> AuthorizationRequest::code_from(const std::string& redirect_url) const {
    const auto parameters = parse_query(redirect_url);

    const auto returned_state = parameters.find("state");
    if (returned_state == parameters.end() || returned_state->second != state) {
        return make_error<std::string>(
            ErrorCode::Unauthorized,
            "authorization state does not match the request; the response is not ours");
    }

    const auto error = parameters.find("error");
    if (error != parameters.end()) {
        const auto description = parameters.find("error_description");
        return make_error<std::string>(
            ErrorCode::Unauthorized,
            "authorization failed: "
                + (description == parameters.end() ? error->second : description->second));
    }

    const auto code = parameters.find("code");
    if (code == parameters.end()) {
        return make_error<std::string>(ErrorCode::Unauthorized, "the redirect carried no code");
    }
    return Result<std::string>::ok(code->second);
}

Result<ServerMetadata> discover(const std::string& issuer, Transport& transport) {
    std::string base = issuer;
    while (!base.empty() && base.back() == '/') {
        base.pop_back();
    }

    std::string last_problem = "no discovery document";
    for (const std::string& suffix :
         {std::string("/.well-known/openid-configuration"),
          std::string("/.well-known/oauth-authorization-server")}) {
        Request request;
        request.method = "GET";
        request.url = base + suffix;
        request.headers.emplace_back("accept", "application/json");

        auto response = transport.send(request);
        if (!response) {
            last_problem = request.url + ": " + response.error().message;
            continue;
        }
        if (response.value().status != 200) {
            last_problem = request.url + ": HTTP " + std::to_string(response.value().status);
            continue;
        }

        auto document = Json::parse(response.value().body_text());
        if (!document) {
            last_problem = request.url + ": " + document.error().message;
            continue;
        }

        const std::string declared = document.value().optional_string("issuer").value_or("");
        if (declared != issuer) {
            return make_error<ServerMetadata>(
                ErrorCode::Unauthorized,
                "discovery at " + request.url + " claims issuer " + declared + ", expected "
                    + issuer);
        }

        const auto authorization = document.value().optional_string("authorization_endpoint");
        const auto token = document.value().optional_string("token_endpoint");
        if (!authorization || !token) {
            return make_error<ServerMetadata>(ErrorCode::Unsupported,
                                              request.url + " omits the endpoints PKCE needs");
        }

        ServerMetadata metadata;
        metadata.issuer = declared;
        metadata.authorization_endpoint = *authorization;
        metadata.token_endpoint = *token;
        metadata.code_challenge_methods_supported =
            string_list(document.value().find("code_challenge_methods_supported"));
        return Result<ServerMetadata>::ok(std::move(metadata));
    }

    return make_error<ServerMetadata>(ErrorCode::Unauthorized,
                                      "cannot discover " + issuer + ": " + last_problem);
}

Result<AuthorizationRequest> begin_authorization(
    const ServerMetadata& metadata, const OAuthClient& client, const std::string& scope,
    const std::map<std::string, std::string>& extra_parameters) {
    const auto& methods = metadata.code_challenge_methods_supported;
    if (!methods.empty() && std::find(methods.begin(), methods.end(), "S256") == methods.end()) {
        return make_error<AuthorizationRequest>(
            ErrorCode::Unsupported, metadata.issuer + " does not support PKCE S256");
    }
    if (std::find(client.flows.begin(), client.flows.end(), "authorization_code_pkce")
        == client.flows.end()) {
        return make_error<AuthorizationRequest>(
            ErrorCode::Unsupported,
            "client " + client.client_id + " is not registered for authorization_code_pkce");
    }

    auto code_verifier = random_url_safe(32);
    if (!code_verifier) {
        return Result<AuthorizationRequest>::fail(code_verifier.error());
    }
    auto state = random_url_safe(16);
    if (!state) {
        return Result<AuthorizationRequest>::fail(state.error());
    }

    const auto digest =
        detail::sha256(code_verifier.value().data(), code_verifier.value().size());

    std::vector<std::pair<std::string, std::string>> parameters = {
        {"response_type", "code"},
        {"client_id", client.client_id},
        {"redirect_uri", client.redirect_uri},
        {"scope", scope},
        {"state", state.value()},
        {"code_challenge", base64_url(digest.data(), digest.size())},
        {"code_challenge_method", "S256"},
    };
    for (const auto& extra : extra_parameters) {
        parameters.push_back(extra);
    }

    AuthorizationRequest request;
    request.url = metadata.authorization_endpoint
                  + (metadata.authorization_endpoint.find('?') == std::string::npos ? "?" : "&")
                  + form(parameters);
    request.state = state.value();
    request.code_verifier = code_verifier.value();
    return Result<AuthorizationRequest>::ok(std::move(request));
}

Result<RefreshableToken> complete_authorization_with_refresh(const ServerMetadata& metadata,
                                                             const OAuthClient& client,
                                                             const AuthorizationRequest& request,
                                                             const std::string& code,
                                                             Transport& transport) {
    // A public client has no secret. Sending one would mean it had been shipped
    // to wherever this code runs.
    const std::string body = form({
        {"grant_type", "authorization_code"},
        {"code", code},
        {"redirect_uri", client.redirect_uri},
        {"client_id", client.client_id},
        {"code_verifier", request.code_verifier},
    });

    Request exchange;
    exchange.method = "POST";
    exchange.url = metadata.token_endpoint;
    exchange.headers.emplace_back("content-type", "application/x-www-form-urlencoded");
    exchange.headers.emplace_back("accept", "application/json");
    exchange.body.assign(body.begin(), body.end());

    auto response = transport.send(exchange);
    if (!response) {
        return Result<RefreshableToken>::fail(response.error());
    }

    auto parsed = Json::parse(response.value().body_text());
    const Json payload = parsed.has_value() ? parsed.value() : Json::object({});

    if (response.value().status / 100 != 2) {
        const std::string detail =
            payload.optional_string("error_description")
                .value_or(payload.optional_string("error").value_or(
                    "HTTP " + std::to_string(response.value().status)));
        return make_error<RefreshableToken>(ErrorCode::Unauthorized,
                                            "token exchange failed: " + detail);
    }

    const auto token = payload.optional_string("access_token");
    if (!token) {
        return make_error<RefreshableToken>(ErrorCode::Unauthorized,
                                            "the token response carried no access_token");
    }

    RefreshableToken result;
    result.access.token = *token;
    result.access.token_type = payload.optional_string("token_type").value_or("bearer");
    result.access.scope = payload.optional_string("scope");
    if (const Json* expires = payload.find("expires_in")) {
        if (auto seconds = expires->as_integer()) {
            result.access.expires_in_seconds = static_cast<int>(*seconds);
        }
    }
    result.refresh_token = payload.optional_string("refresh_token");
    return Result<RefreshableToken>::ok(std::move(result));
}

Result<AccessToken> complete_authorization(const ServerMetadata& metadata,
                                           const OAuthClient& client,
                                           const AuthorizationRequest& request,
                                           const std::string& code, Transport& transport) {
    auto full = complete_authorization_with_refresh(metadata, client, request, code, transport);
    if (!full) {
        return Result<AccessToken>::fail(full.error());
    }
    // The refresh token is dropped here rather than returned.
    return Result<AccessToken>::ok(std::move(full).value().access);
}

}  // namespace auru::pm::oauth

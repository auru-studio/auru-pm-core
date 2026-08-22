#pragma once

#include <map>
#include <memory>
#include <optional>
#include <string>
#include <vector>

#include "auru/pm/result.hpp"
#include "auru/pm/transport.hpp"
#include "auru/pm/types.hpp"

namespace auru::pm {

/// OAuth 2.0 Authorization Code with PKCE, for providers that advertise it.
///
/// Two things here are deliberate and worth reading before changing them.
///
/// **Endpoints come from discovery, never from the provider's health
/// document.** A provider publishes only its issuer; `discover` fetches that
/// issuer's RFC 8414 or OpenID Connect metadata and rejects a document whose
/// `issuer` is not byte-identical to the one asked for. A provider that could
/// name its own token endpoint could name someone else's.
///
/// **The refresh token is discarded by default.** `complete_authorization`
/// returns only a short-lived access token. A caller who is handed a refresh
/// token will eventually store it, and most places it could be stored are worse
/// than not having it at all. `complete_authorization_with_refresh` is the
/// explicit opt-in for callers that have a real secret store.
namespace oauth {

/// The subset of authorization-server metadata this flow needs.
struct ServerMetadata {
    std::string issuer;
    std::string authorization_endpoint;
    std::string token_endpoint;
    std::vector<std::string> code_challenge_methods_supported;
};

/// An access token, held for as long as the caller keeps it.
struct AccessToken {
    std::string token;
    std::string token_type = "bearer";
    std::optional<int> expires_in_seconds;
    std::optional<std::string> scope;
};

/// An access token together with its refresh token.
struct RefreshableToken {
    AccessToken access;
    std::optional<std::string> refresh_token;
};

/// A prepared authorization request.
struct AuthorizationRequest {
    /// Send the user here.
    std::string url;
    /// Echoed back on the redirect; a mismatch means the response is not ours.
    std::string state;
    /// The PKCE secret. Keep it in memory until the redirect returns.
    std::string code_verifier;

    /// Check the redirect's `state` and pull out the code.
    Result<std::string> code_from(const std::string& redirect_url) const;
};

/// Fetch and validate an issuer's authorization-server metadata.
///
/// Tries OpenID Connect discovery, then RFC 8414.
Result<ServerMetadata> discover(const std::string& issuer, Transport& transport);

/// Build an authorization URL and the PKCE secret that completes it.
///
/// Refuses a provider that does not advertise `S256`. A plain challenge is not
/// a fallback worth having: the point of PKCE is that an intercepted
/// authorization code is useless, which a plain challenge does not give you.
Result<AuthorizationRequest> begin_authorization(
    const ServerMetadata& metadata, const OAuthClient& client, const std::string& scope,
    const std::map<std::string, std::string>& extra_parameters = {});

/// Exchange an authorization code for an access token.
///
/// The refresh token, if the provider issued one, is discarded rather than
/// returned. When the access token expires, run the flow again.
Result<AccessToken> complete_authorization(const ServerMetadata& metadata,
                                           const OAuthClient& client,
                                           const AuthorizationRequest& request,
                                           const std::string& code, Transport& transport);

/// Exchange an authorization code, keeping the refresh token.
///
/// Only for callers with a real secret store. A refresh token is a long-lived
/// credential for the whole account; treat it the way you would treat a
/// password.
Result<RefreshableToken> complete_authorization_with_refresh(const ServerMetadata& metadata,
                                                             const OAuthClient& client,
                                                             const AuthorizationRequest& request,
                                                             const std::string& code,
                                                             Transport& transport);

}  // namespace oauth
}  // namespace auru::pm

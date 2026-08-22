use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use auru_pm_protocol::OAuthFlow;
use serde::Deserialize;
use url::Url;

const CONFIG_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub version: u32,
    #[serde(default = "default_provider_id")]
    pub provider_id: String,
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    #[serde(default)]
    pub public_base_url: Option<String>,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "default_requests_per_minute")]
    pub requests_per_minute: u32,
    /// Browser origins permitted to call this server cross-origin.
    ///
    /// Empty by default, which installs no CORS layer at all — a server that
    /// only serves native clients should not answer preflights. Each entry is
    /// a bare origin (`https://dashboard.example.com`), never a wildcard: the
    /// dashboard sends a bearer token in `Authorization`, and an origin
    /// allow-list is what keeps another site from asking the browser to send
    /// requests here on the user's behalf.
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    #[serde(default)]
    pub authentication: AuthenticationConfig,
}

impl ServerConfig {
    pub fn from_toml(source: &str) -> Result<Self, String> {
        let config: Self =
            toml::from_str(source).map_err(|error| format!("server configuration: {error}"))?;
        config.validate()?;
        Ok(config)
    }

    pub fn unauthenticated_legacy(
        listen: SocketAddr,
        data_dir: PathBuf,
        requests_per_minute: u32,
    ) -> Self {
        Self {
            version: CONFIG_VERSION,
            provider_id: default_provider_id(),
            listen,
            public_base_url: None,
            data_dir,
            requests_per_minute,
            allowed_origins: Vec::new(),
            authentication: AuthenticationConfig::default(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != CONFIG_VERSION {
            return Err(format!(
                "unsupported server configuration version {}; expected {CONFIG_VERSION}",
                self.version
            ));
        }
        if self.provider_id.trim().is_empty() {
            return Err("provider_id must not be empty".to_owned());
        }
        for origin in &self.allowed_origins {
            validate_origin(origin)?;
        }
        match &self.authentication {
            AuthenticationConfig::None {
                allow_insecure_non_loopback,
            } => {
                if !self.listen.ip().is_loopback() && !allow_insecure_non_loopback {
                    return Err(
                        "authentication mode `none` may listen only on loopback; set allow_insecure_non_loopback = true only for an explicitly trusted development network"
                            .to_owned(),
                    );
                }
            }
            AuthenticationConfig::OAuth(oauth) => {
                let public_base_url = self.public_base_url.as_deref().ok_or_else(|| {
                    "public_base_url is required when authentication mode is `oauth`".to_owned()
                })?;
                require_https(public_base_url, "public_base_url")?;
                require_https(&oauth.issuer, "authentication.issuer")?;
                let redirect = Url::parse(&oauth.redirect_uri)
                    .map_err(|error| format!("authentication.redirect_uri: {error}"))?;
                if redirect.scheme() != "http"
                    || redirect.host_str() != Some("127.0.0.1")
                    || redirect.port().is_none()
                    || redirect.query().is_some()
                    || redirect.fragment().is_some()
                {
                    return Err(
                        "authentication.redirect_uri must use a fixed http://127.0.0.1:<port>/ callback"
                            .to_owned(),
                    );
                }
                if oauth.audience.trim().is_empty() {
                    return Err("authentication.audience must not be empty".to_owned());
                }
                if oauth.desktop_client_id.trim().is_empty() {
                    return Err("authentication.desktop_client_id must not be empty".to_owned());
                }
                if oauth.required_scope.trim().is_empty() {
                    return Err("authentication.required_scope must not be empty".to_owned());
                }
                if oauth.required_scope.split_whitespace().count() != 1
                    || oauth.required_scope != oauth.required_scope.trim()
                {
                    return Err(
                        "authentication.required_scope must contain exactly one scope token"
                            .to_owned(),
                    );
                }
                if let Some(browser) = &oauth.browser_client {
                    if browser.client_id.trim().is_empty() {
                        return Err(
                            "authentication.browser_client.client_id must not be empty".to_owned()
                        );
                    }
                    if browser.client_id == oauth.desktop_client_id {
                        return Err(
                            "authentication.browser_client.client_id must differ from desktop_client_id"
                                .to_owned(),
                        );
                    }
                    validate_browser_redirect(&browser.redirect_uri)?;
                    if browser.flows.is_empty() {
                        return Err(
                            "authentication.browser_client.flows must declare at least one flow"
                                .to_owned(),
                        );
                    }
                    if self.allowed_origins.is_empty() {
                        return Err(
                            "authentication.browser_client is configured but allowed_origins is empty; the dashboard's origin could not reach this server"
                                .to_owned(),
                        );
                    }
                }
                if oauth.flows.is_empty() {
                    return Err("authentication.flows must declare at least one flow".to_owned());
                }
                if oauth
                    .legacy_owner_subject
                    .as_ref()
                    .is_some_and(|subject| subject.trim().is_empty())
                {
                    return Err("authentication.legacy_owner_subject must not be empty".to_owned());
                }
                if oauth.display_name_claims.is_empty()
                    || oauth
                        .display_name_claims
                        .iter()
                        .any(|claim| claim.trim().is_empty())
                {
                    return Err(
                        "authentication.display_name_claims must contain at least one non-empty claim name"
                            .to_owned(),
                    );
                }
                if oauth.email_claim.trim().is_empty() {
                    return Err("authentication.email_claim must not be empty".to_owned());
                }
                if let TokenValidationConfig::Introspection {
                    endpoint,
                    client_id,
                    client_secret_env,
                } = &oauth.validation
                {
                    if let Some(endpoint) = endpoint {
                        require_https(endpoint, "authentication.validation.endpoint")?;
                    }
                    if client_id.trim().is_empty() {
                        return Err(
                            "authentication.validation.client_id must not be empty".to_owned()
                        );
                    }
                    if client_secret_env.trim().is_empty() {
                        return Err(
                            "authentication.validation.client_secret_env must name an environment variable"
                                .to_owned(),
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthenticationConfig {
    None {
        #[serde(default)]
        allow_insecure_non_loopback: bool,
    },
    #[serde(rename = "oauth")]
    OAuth(Box<OAuthConfig>),
}

impl Default for AuthenticationConfig {
    fn default() -> Self {
        Self::None {
            allow_insecure_non_loopback: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthConfig {
    pub issuer: String,
    pub audience: String,
    pub desktop_client_id: String,
    #[serde(default = "default_required_scope")]
    pub required_scope: String,
    pub redirect_uri: String,
    #[serde(default = "default_oauth_flows")]
    pub flows: Vec<OAuthFlow>,
    /// A second public client for a browser dashboard, if one exists.
    ///
    /// Separate from the desktop registration rather than a shared client id:
    /// an identity provider's redirect allow-list is per client, and a native
    /// loopback redirect and an https single-page redirect cannot both live on
    /// one entry without widening it past either app's needs.
    #[serde(default)]
    pub browser_client: Option<BrowserClientConfig>,
    #[serde(default)]
    pub legacy_owner_subject: Option<String>,
    pub validation: TokenValidationConfig,
    #[serde(default = "default_display_name_claims")]
    pub display_name_claims: Vec<String>,
    #[serde(default = "default_email_claim")]
    pub email_claim: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "strategy", rename_all = "snake_case", deny_unknown_fields)]
pub enum TokenValidationConfig {
    Jwt,
    Introspection {
        #[serde(default)]
        endpoint: Option<String>,
        client_id: String,
        client_secret_env: String,
    },
}

fn default_listen() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4242)
}

fn default_provider_id() -> String {
    "auru-pm-server".to_owned()
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("auru-pm-server-data")
}

fn default_requests_per_minute() -> u32 {
    600
}

fn default_required_scope() -> String {
    "openid".to_owned()
}

fn default_oauth_flows() -> Vec<OAuthFlow> {
    vec![OAuthFlow::AuthorizationCodePkce]
}

fn default_display_name_claims() -> Vec<String> {
    vec!["name".to_owned(), "preferred_username".to_owned()]
}

fn default_email_claim() -> String {
    "email".to_owned()
}

/// A browser single-page app registered as a second public client.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserClientConfig {
    pub client_id: String,
    pub redirect_uri: String,
    #[serde(default = "default_oauth_flows")]
    pub flows: Vec<OAuthFlow>,
}

/// Whether `host` is a loopback name, for which plain http is acceptable.
fn is_loopback_host(url: &Url) -> bool {
    match url.host_str() {
        Some("localhost") => true,
        Some(host) => host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback()),
        None => false,
    }
}

/// A browser redirect is an https URL the app serves, not a loopback callback.
fn validate_browser_redirect(value: &str) -> Result<(), String> {
    let field = "authentication.browser_client.redirect_uri";
    let url = Url::parse(value).map_err(|error| format!("{field}: {error}"))?;
    if url.host_str().is_none() {
        return Err(format!("{field} must name a host"));
    }
    // http is tolerated only for a loopback dev server; anything else on the
    // open internet would put an authorization code on the wire in clear.
    if url.scheme() != "https" && !(url.scheme() == "http" && is_loopback_host(&url)) {
        return Err(format!(
            "{field} must use https, or http on loopback for local development"
        ));
    }
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(format!(
            "{field} must not contain credentials or a fragment"
        ));
    }
    Ok(())
}

/// A CORS allow-list entry is a bare origin: scheme, host, optional port.
fn validate_origin(value: &str) -> Result<(), String> {
    let field = "allowed_origins";
    if value == "*" {
        return Err(format!(
            "{field} must name explicit origins; `*` would let any site drive authenticated requests from a user's browser"
        ));
    }
    let url = Url::parse(value).map_err(|error| format!("{field}: {value:?}: {error}"))?;
    if url.host_str().is_none() {
        return Err(format!("{field}: {value:?} must name a host"));
    }
    if url.scheme() != "https" && !(url.scheme() == "http" && is_loopback_host(&url)) {
        return Err(format!(
            "{field}: {value:?} must use https, or http on loopback for local development"
        ));
    }
    if !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() {
        return Err(format!(
            "{field}: {value:?} must be a bare origin with no path, query, or fragment"
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("{field}: {value:?} must not contain credentials"));
    }
    Ok(())
}

fn require_https(value: &str, field: &str) -> Result<(), String> {
    let url = Url::parse(value).map_err(|error| format!("{field}: {error}"))?;
    if url.scheme() != "https" || url.host_str().is_none() {
        return Err(format!("{field} must use https"));
    }
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(format!(
            "{field} must not contain credentials or a fragment"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standards_based_jwt_configuration_should_parse() {
        let config = ServerConfig::from_toml(
            r#"
version = 1
listen = "127.0.0.1:4242"
public_base_url = "https://pm.example.com"
data_dir = "./server-data"
requests_per_minute = 600

[authentication]
mode = "oauth"
issuer = "https://identity.example.com"
audience = "auru-pm"
desktop_client_id = "auru-desktop"
required_scope = "openid"
redirect_uri = "http://127.0.0.1:43827/oauth/callback"
flows = ["authorization_code_pkce", "device_authorization"]
legacy_owner_subject = "user_existing"

[authentication.validation]
strategy = "jwt"
"#,
        )
        .expect("valid OAuth server configuration");

        let AuthenticationConfig::OAuth(oauth) = config.authentication else {
            panic!("OAuth configuration");
        };
        assert_eq!(oauth.issuer, "https://identity.example.com");
        assert_eq!(oauth.required_scope, "openid");
        assert_eq!(
            oauth.flows,
            vec![
                OAuthFlow::AuthorizationCodePkce,
                OAuthFlow::DeviceAuthorization
            ]
        );
        assert!(matches!(oauth.validation, TokenValidationConfig::Jwt));
    }

    #[test]
    fn incomplete_oauth_configuration_should_fail_closed() {
        let error = ServerConfig::from_toml(
            r#"
version = 1
public_base_url = "https://pm.example.com"

[authentication]
mode = "oauth"
issuer = "https://identity.example.com"
"#,
        )
        .expect_err("audience, client id, redirect, and validation are mandatory");

        assert!(error.to_string().contains("audience"), "{error}");
    }

    #[test]
    fn shipped_oauth_example_should_remain_valid() {
        ServerConfig::from_toml(include_str!("../server.example.toml"))
            .expect("shipped server configuration");
    }

    #[test]
    fn unauthenticated_non_loopback_listener_should_require_an_unsafe_override() {
        let error = ServerConfig::from_toml(
            r#"
version = 1
listen = "0.0.0.0:4242"
[authentication]
mode = "none"
"#,
        )
        .expect_err("unauthenticated public listener");
        assert!(error.contains("loopback"), "{error}");
    }
}

#[cfg(test)]
mod browser_client_tests {
    use super::*;

    fn config(extra: &str) -> Result<ServerConfig, String> {
        ServerConfig::from_toml(&format!(
            r#"
version = 1
provider_id = "studio-pm"
public_base_url = "https://pm.example.com"
{extra}
[authentication.validation]
strategy = "jwt"
"#
        ))
    }

    const OAUTH: &str = r#"
[authentication]
mode = "oauth"
issuer = "https://identity.example.com"
audience = "auru-pm"
desktop_client_id = "desktop"
redirect_uri = "http://127.0.0.1:43827/oauth/callback"
"#;

    #[test]
    fn a_wildcard_origin_is_refused() {
        let error = config(&format!("allowed_origins = [\"*\"]\n{OAUTH}")).unwrap_err();
        assert!(error.contains("explicit origins"), "{error}");
    }

    #[test]
    fn plain_http_origins_are_refused_off_loopback() {
        let error = config(&format!(
            "allowed_origins = [\"http://dashboard.example.com\"]\n{OAUTH}"
        ))
        .unwrap_err();
        assert!(error.contains("must use https"), "{error}");
    }

    #[test]
    fn loopback_http_origins_are_allowed_for_local_development() {
        let config = config(&format!(
            "allowed_origins = [\"http://localhost:5173\", \"http://127.0.0.1:5173\"]\n{OAUTH}"
        ))
        .unwrap();
        assert_eq!(config.allowed_origins.len(), 2);
    }

    #[test]
    fn an_origin_with_a_path_is_refused() {
        let error = config(&format!(
            "allowed_origins = [\"https://dashboard.example.com/app\"]\n{OAUTH}"
        ))
        .unwrap_err();
        assert!(error.contains("bare origin"), "{error}");
    }

    #[test]
    fn the_browser_client_may_not_reuse_the_desktop_client_id() {
        let error = config(&format!(
            r#"allowed_origins = ["https://dashboard.example.com"]
{OAUTH}
[authentication.browser_client]
client_id = "desktop"
redirect_uri = "https://dashboard.example.com/oauth/callback"
"#
        ))
        .unwrap_err();
        assert!(
            error.contains("must differ from desktop_client_id"),
            "{error}"
        );
    }

    #[test]
    fn a_browser_client_without_allowed_origins_is_refused() {
        // Registering the client but forgetting the origin list produces a
        // dashboard that authenticates and then cannot reach the server at all.
        let error = config(&format!(
            r#"{OAUTH}
[authentication.browser_client]
client_id = "dashboard"
redirect_uri = "https://dashboard.example.com/oauth/callback"
"#
        ))
        .unwrap_err();
        assert!(error.contains("allowed_origins is empty"), "{error}");
    }

    #[test]
    fn a_browser_redirect_must_not_be_plain_http_off_loopback() {
        let error = config(&format!(
            r#"allowed_origins = ["https://dashboard.example.com"]
{OAUTH}
[authentication.browser_client]
client_id = "dashboard"
redirect_uri = "http://dashboard.example.com/oauth/callback"
"#
        ))
        .unwrap_err();
        assert!(error.contains("must use https"), "{error}");
    }
}

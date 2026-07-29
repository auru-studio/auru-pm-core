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

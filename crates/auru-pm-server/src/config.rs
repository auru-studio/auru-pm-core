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
    /// TLS terminated by this server rather than by a proxy in front of it.
    ///
    /// Absent by default, which is what a deployment wants: the reverse proxy
    /// owns the certificate and this server listens on private HTTP. The
    /// section exists for local development against a device, where there is
    /// no proxy and Android refuses cleartext HTTP outright.
    #[serde(default)]
    pub tls: Option<TlsConfig>,
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
            tls: None,
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
        if let Some(tls) = &self.tls {
            tls.validate()?;
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
                if let Some(mobile) = &oauth.mobile_client {
                    if mobile.client_id.trim().is_empty() {
                        return Err(
                            "authentication.mobile_client.client_id must not be empty".to_owned()
                        );
                    }
                    if mobile.client_id == oauth.desktop_client_id {
                        return Err(
                            "authentication.mobile_client.client_id must differ from desktop_client_id"
                                .to_owned(),
                        );
                    }
                    if oauth
                        .browser_client
                        .as_ref()
                        .is_some_and(|browser| browser.client_id == mobile.client_id)
                    {
                        return Err(
                            "authentication.mobile_client.client_id must differ from browser_client.client_id"
                                .to_owned(),
                        );
                    }
                    validate_mobile_redirect(&mobile.redirect_uri)?;
                    if mobile.flows.is_empty() {
                        return Err(
                            "authentication.mobile_client.flows must declare at least one flow"
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

/// Where this server's certificate and key come from.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum TlsConfig {
    /// A self-signed authority kept in the data directory.
    ///
    /// The leaf is reissued on every start so it always covers the addresses
    /// this run listens on; the authority is not, because it is the file a
    /// client was told to trust. Development only — nothing outside the
    /// machine that minted it has any reason to accept this chain.
    DevelopmentCertificate {
        /// Names to cover beyond the loopback aliases, this run's listen
        /// address, and — for a wildcard listener — the machine's own
        /// interface addresses. A hostname a phone resolves over mDNS, say.
        #[serde(default)]
        subject_alt_names: Vec<String>,
        /// Where the authority itself lives. Defaults to a per-user directory,
        /// deliberately not `data_dir`: a client bakes the anchor in at build
        /// time, so an authority per data directory breaks an installed client
        /// the moment `--data-dir` changes.
        #[serde(default)]
        authority_directory: Option<PathBuf>,
    },
    /// A chain and key issued elsewhere: mkcert, an internal CA, staging.
    Files {
        /// PEM chain, leaf first. Intermediates belong here too — a client
        /// that cannot build a path to its anchor rejects the connection.
        certificate: PathBuf,
        private_key: PathBuf,
    },
}

impl TlsConfig {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::DevelopmentCertificate {
                subject_alt_names, ..
            } => {
                for name in subject_alt_names {
                    validate_subject_alt_name(name)?;
                }
            }
            Self::Files {
                certificate,
                private_key,
            } => {
                for (field, path) in [
                    ("tls.certificate", certificate),
                    ("tls.private_key", private_key),
                ] {
                    if path.as_os_str().is_empty() {
                        return Err(format!("{field} must name a file"));
                    }
                }
            }
        }
        Ok(())
    }
}

/// A subject alternative name is a bare host or IP — no scheme, port, or path.
///
/// Checked here rather than left to the certificate generator because the
/// generator's own error arrives after the data directory has been touched and
/// names the ASN.1 encoder, not the line of TOML that is wrong.
fn validate_subject_alt_name(value: &str) -> Result<(), String> {
    let field = "tls.subject_alt_names";
    if value.is_empty() {
        return Err(format!("{field} must not contain an empty name"));
    }
    if value.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    if value.contains([':', '/', '@', ' ']) {
        return Err(format!(
            "{field}: {value:?} must be a bare hostname or IP address, with no scheme, port, or path"
        ));
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '.' || character == '-')
    {
        return Err(format!("{field}: {value:?} is not a valid hostname"));
    }
    Ok(())
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
    /// A third public client for the phone apps, if the identity provider
    /// registers one.
    ///
    /// Optional because the phones work without it: the device-authorization
    /// grant needs no redirect URI, so they can share the native registration.
    /// Registering this splits them — the phones send their own `client_id`,
    /// and browser sign-in on a phone gets the custom-scheme or universal-link
    /// redirect that a loopback callback cannot provide (§12.1 of the mobile
    /// spec).
    #[serde(default)]
    pub mobile_client: Option<MobileClientConfig>,
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

/// The phone apps registered as their own public client.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MobileClientConfig {
    pub client_id: String,
    pub redirect_uri: String,
    #[serde(default = "default_mobile_flows")]
    pub flows: Vec<OAuthFlow>,
}

/// Phones lead with the device grant and add browser sign-in where the
/// redirect works, so the default permits both.
fn default_mobile_flows() -> Vec<OAuthFlow> {
    vec![
        OAuthFlow::AuthorizationCodePkce,
        OAuthFlow::DeviceAuthorization,
    ]
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

/// A mobile redirect is a custom scheme the app claims, or an https universal
/// link — never plain http, which no phone platform hands to an app.
fn validate_mobile_redirect(value: &str) -> Result<(), String> {
    let field = "authentication.mobile_client.redirect_uri";
    let url = Url::parse(value).map_err(|error| format!("{field}: {error}"))?;
    if url.scheme() == "http" {
        return Err(format!(
            "{field} must use a custom scheme or an https universal link, not plain http"
        ));
    }
    if url.scheme() == "https" && url.host_str().is_none() {
        return Err(format!("{field} must name a host when using https"));
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
mod tls_tests {
    use super::*;

    fn config(tls: &str) -> Result<ServerConfig, String> {
        ServerConfig::from_toml(&format!("version = 1\nlisten = \"127.0.0.1:4242\"\n{tls}"))
    }

    #[test]
    fn a_development_certificate_needs_no_further_configuration() {
        let config = config("[tls]\nmode = \"development_certificate\"\n")
            .expect("a development certificate is self-contained");
        assert!(matches!(
            config.tls,
            Some(TlsConfig::DevelopmentCertificate { .. })
        ));
    }

    #[test]
    fn omitting_the_section_leaves_the_server_on_plain_http() {
        // The deployment shape: a reverse proxy in front owns TLS.
        let config = config("").expect("TLS is optional");
        assert!(config.tls.is_none());
    }

    #[test]
    fn a_supplied_chain_requires_both_halves() {
        let error = config("[tls]\nmode = \"files\"\ncertificate = \"chain.pem\"\n")
            .expect_err("a certificate without its key cannot serve anything");
        assert!(error.contains("private_key"), "{error}");
    }

    #[test]
    fn a_subject_alternative_name_may_not_carry_a_scheme_or_port() {
        for name in ["https://studio.local", "studio.local:4242"] {
            let error = config(&format!(
                "[tls]\nmode = \"development_certificate\"\nsubject_alt_names = [\"{name}\"]\n"
            ))
            .expect_err("a SAN is a bare host, and a certificate never covers a port");
            assert!(error.contains("bare hostname"), "{name}: {error}");
        }
    }

    #[test]
    fn an_ip_address_is_a_valid_subject_alternative_name() {
        config("[tls]\nmode = \"development_certificate\"\nsubject_alt_names = [\"::1\", \"10.0.2.2\"]\n")
            .expect("IPv6 contains colons but is still a name a certificate can carry");
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

#[cfg(test)]
mod mobile_client_tests {
    use super::*;

    fn config(extra: &str) -> Result<ServerConfig, String> {
        ServerConfig::from_toml(&format!(
            r#"
version = 1
provider_id = "studio-pm"
public_base_url = "https://pm.example.com"
[authentication]
mode = "oauth"
issuer = "https://identity.example.com"
audience = "auru-pm"
desktop_client_id = "desktop"
redirect_uri = "http://127.0.0.1:43827/oauth/callback"
{extra}
[authentication.validation]
strategy = "jwt"
"#
        ))
    }

    #[test]
    fn a_custom_scheme_redirect_is_accepted_and_the_flows_default_to_both_grants() {
        let config = config(
            r#"[authentication.mobile_client]
client_id = "phones"
redirect_uri = "studio.auru.pm:/oauth/callback"
"#,
        )
        .unwrap();
        let AuthenticationConfig::OAuth(oauth) = config.authentication else {
            panic!("OAuth configuration");
        };
        let mobile = oauth.mobile_client.expect("mobile client");
        assert_eq!(
            mobile.flows,
            vec![
                OAuthFlow::AuthorizationCodePkce,
                OAuthFlow::DeviceAuthorization
            ],
            "phones need the device grant to pick this registration at all"
        );
    }

    #[test]
    fn a_universal_link_redirect_is_accepted() {
        config(
            r#"[authentication.mobile_client]
client_id = "phones"
redirect_uri = "https://pm.example.com/app/oauth/callback"
"#,
        )
        .unwrap();
    }

    #[test]
    fn the_mobile_client_may_not_reuse_the_desktop_client_id() {
        let error = config(
            r#"[authentication.mobile_client]
client_id = "desktop"
redirect_uri = "studio.auru.pm:/oauth/callback"
"#,
        )
        .unwrap_err();
        assert!(
            error.contains("must differ from desktop_client_id"),
            "{error}"
        );
    }

    #[test]
    fn the_mobile_client_may_not_reuse_the_browser_client_id() {
        let error = ServerConfig::from_toml(
            r#"
version = 1
provider_id = "studio-pm"
public_base_url = "https://pm.example.com"
allowed_origins = ["https://dashboard.example.com"]
[authentication]
mode = "oauth"
issuer = "https://identity.example.com"
audience = "auru-pm"
desktop_client_id = "desktop"
redirect_uri = "http://127.0.0.1:43827/oauth/callback"
[authentication.browser_client]
client_id = "dashboard"
redirect_uri = "https://dashboard.example.com/oauth/callback"
[authentication.mobile_client]
client_id = "dashboard"
redirect_uri = "studio.auru.pm:/oauth/callback"
[authentication.validation]
strategy = "jwt"
"#,
        )
        .unwrap_err();
        assert!(
            error.contains("must differ from browser_client.client_id"),
            "{error}"
        );
    }

    #[test]
    fn a_mobile_redirect_must_not_be_plain_http() {
        // Unlike a dashboard there is no loopback development exception: no
        // phone platform hands an http URL to an app.
        let error = config(
            r#"[authentication.mobile_client]
client_id = "phones"
redirect_uri = "http://127.0.0.1:8080/oauth/callback"
"#,
        )
        .unwrap_err();
        assert!(error.contains("custom scheme"), "{error}");
    }
}

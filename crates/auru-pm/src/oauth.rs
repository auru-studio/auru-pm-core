//! OAuth 2.0 Device Authorization Grant (RFC 8628) client.
//!
//! Used by the Auru-hosted reference provider (and any third-party provider
//! that advertises `auth_methods: ["oauth_device_code"]` from `/v1/health`).
//!
//! ## Flow
//! 1. POST `/v1/auth/device/code` → [`DeviceCodeResponse`] (shown in UI)
//! 2. User visits `verification_uri` in a browser and authenticates.
//! 3. Client polls POST `/v1/auth/token` at `interval`-second pace.
//! 4. Server returns `access_token` once the user approves.

use std::sync::mpsc;
use std::time::Duration;

use auru_pm_protocol::{OAuthClientConfiguration, OAuthFlow};
use base64::Engine as _;
use rand::RngCore as _;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use url::Url;

use crate::token_store::ProviderCredential;

/// Response from the device authorization endpoint (`POST /v1/auth/device/code`).
#[derive(Clone, Debug, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    /// URI with user_code embedded — ideal for QR display.
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    /// Seconds until `device_code` expires.
    #[serde(default = "default_expires_in")]
    pub expires_in: u64,
    /// Minimum seconds between token poll attempts.
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_expires_in() -> u64 {
    300
}
fn default_interval() -> u64 {
    5
}

/// Streaming progress updates sent to the UI thread during the OAuth flow.
#[derive(Debug)]
pub enum OAuthProgress {
    /// Open this authorization URL in the person's ordinary browser.
    AuthorizationUrl(String),
    /// Device code received — UI should show `user_code` and `verification_uri`.
    DeviceCode(DeviceCodeResponse),
    /// Still waiting for the user to authenticate.
    Pending,
    /// Successfully authenticated; carries the bearer token.
    Token(String),
    /// Successfully authenticated through standards-based discovery; carries
    /// the access token and any refresh-token rotation state.
    Credential(ProviderCredential),
    /// Device code expired before the user authenticated.
    Expired,
    /// User explicitly denied the authorization.
    AccessDenied,
    /// Error during the flow.
    Error(String),
}

#[derive(Serialize)]
struct DeviceCodeRequest<'a> {
    client_id: &'a str,
}

#[derive(Serialize)]
struct TokenPollRequest<'a> {
    grant_type: &'static str,
    device_code: &'a str,
    client_id: &'a str,
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AuthorizationServerMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    #[serde(default)]
    pub device_authorization_endpoint: Option<String>,
    #[serde(default)]
    pub code_challenge_methods_supported: Vec<String>,
    #[serde(default)]
    pub jwks_uri: Option<String>,
    #[serde(default)]
    pub introspection_endpoint: Option<String>,
}

/// Start an OAuth flow described by a PM server's public health metadata.
///
/// Authorization Code + PKCE is preferred. Direct RFC 8628 device
/// authorization is used only when it is the sole configured flow.
pub fn start_standard_oauth_flow(
    configuration: OAuthClientConfiguration,
) -> mpsc::Receiver<OAuthProgress> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        runtime.block_on(async move {
            let client = match Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
            {
                Ok(client) => client,
                Err(error) => {
                    let _ = tx.send(OAuthProgress::Error(format!(
                        "create OAuth client: {error}"
                    )));
                    return;
                }
            };
            let metadata = match discover_authorization_server(&client, &configuration.issuer).await
            {
                Ok(metadata) => metadata,
                Err(error) => {
                    let _ = tx.send(OAuthProgress::Error(error));
                    return;
                }
            };
            if configuration
                .flows
                .contains(&OAuthFlow::AuthorizationCodePkce)
            {
                run_authorization_code_flow(&client, &configuration, &metadata, &tx).await;
            } else if configuration
                .flows
                .contains(&OAuthFlow::DeviceAuthorization)
            {
                run_discovered_device_flow(&client, &configuration, &metadata, &tx).await;
            } else {
                let _ = tx.send(OAuthProgress::Error(
                    "the provider did not advertise a supported OAuth flow".to_owned(),
                ));
            }
        });
    });
    rx
}

/// Discover and validate one exact OAuth/OIDC issuer.
pub async fn discover_authorization_server(
    client: &Client,
    issuer: &str,
) -> Result<AuthorizationServerMetadata, String> {
    let configured = issuer;
    let configured_url =
        Url::parse(configured).map_err(|error| format!("invalid OAuth issuer: {error}"))?;
    if configured_url.scheme() != "https" || configured_url.host_str().is_none() {
        return Err("the OAuth issuer must use https".to_owned());
    }
    let oidc = format!(
        "{}/.well-known/openid-configuration",
        configured.trim_end_matches('/')
    );
    let oauth = oauth_metadata_url(configured)?;
    let mut last_error = String::new();
    for url in [oidc, oauth] {
        match client.get(&url).send().await {
            Ok(response) if response.status().is_success() => {
                let metadata: AuthorizationServerMetadata = response
                    .json()
                    .await
                    .map_err(|error| format!("decode authorization server metadata: {error}"))?;
                if metadata.issuer != configured {
                    return Err(format!(
                        "authorization server metadata issuer `{}` does not match `{configured}`",
                        metadata.issuer
                    ));
                }
                for (name, endpoint) in [
                    (
                        "authorization_endpoint",
                        metadata.authorization_endpoint.as_str(),
                    ),
                    ("token_endpoint", metadata.token_endpoint.as_str()),
                ] {
                    require_https_endpoint(endpoint, name)?;
                }
                if let Some(endpoint) = metadata.device_authorization_endpoint.as_deref() {
                    require_https_endpoint(endpoint, "device_authorization_endpoint")?;
                }
                if let Some(endpoint) = metadata.jwks_uri.as_deref() {
                    require_https_endpoint(endpoint, "jwks_uri")?;
                }
                if let Some(endpoint) = metadata.introspection_endpoint.as_deref() {
                    require_https_endpoint(endpoint, "introspection_endpoint")?;
                }
                return Ok(metadata);
            }
            Ok(response) => {
                last_error = format!("HTTP {}", response.status());
            }
            Err(error) => last_error = error.to_string(),
        }
    }
    Err(format!(
        "discover authorization server `{configured}`: {last_error}"
    ))
}

fn require_https_endpoint(value: &str, name: &str) -> Result<(), String> {
    let endpoint = Url::parse(value).map_err(|error| format!("invalid OAuth {name}: {error}"))?;
    if endpoint.scheme() != "https" || endpoint.host_str().is_none() {
        return Err(format!("OAuth {name} must use https"));
    }
    if !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(format!(
            "OAuth {name} must not contain credentials or a fragment"
        ));
    }
    Ok(())
}

fn oauth_metadata_url(issuer: &str) -> Result<String, String> {
    let issuer = Url::parse(issuer).map_err(|error| format!("invalid OAuth issuer: {error}"))?;
    let mut metadata = issuer.clone();
    let issuer_path = issuer.path().trim_start_matches('/');
    metadata.set_path(&format!(
        "/.well-known/oauth-authorization-server/{issuer_path}"
    ));
    metadata.set_query(None);
    metadata.set_fragment(None);
    Ok(metadata.to_string().trim_end_matches('/').to_owned())
}

async fn run_authorization_code_flow(
    client: &Client,
    configuration: &OAuthClientConfiguration,
    metadata: &AuthorizationServerMetadata,
    tx: &mpsc::Sender<OAuthProgress>,
) {
    if !metadata.code_challenge_methods_supported.is_empty()
        && !metadata
            .code_challenge_methods_supported
            .iter()
            .any(|method| method == "S256")
    {
        let _ = tx.send(OAuthProgress::Error(
            "the authorization server does not support PKCE S256".to_owned(),
        ));
        return;
    }
    let redirect = match Url::parse(&configuration.redirect_uri) {
        Ok(url)
            if url.scheme() == "http"
                && url.host_str() == Some("127.0.0.1")
                && url.port().is_some()
                && url.query().is_none()
                && url.fragment().is_none() =>
        {
            url
        }
        _ => {
            let _ = tx.send(OAuthProgress::Error(
                "the provider redirect URI is not a fixed 127.0.0.1 loopback callback".to_owned(),
            ));
            return;
        }
    };
    let listener = match TcpListener::bind((
        std::net::Ipv4Addr::LOCALHOST,
        redirect.port().expect("checked above"),
    ))
    .await
    {
        Ok(listener) => listener,
        Err(error) => {
            let _ = tx.send(OAuthProgress::Error(format!(
                "listen for the OAuth callback: {error}"
            )));
            return;
        }
    };

    let verifier = random_urlsafe(64);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let state = random_urlsafe(32);
    let mut authorization_url = match Url::parse(&metadata.authorization_endpoint) {
        Ok(url) => url,
        Err(error) => {
            let _ = tx.send(OAuthProgress::Error(format!(
                "invalid authorization endpoint: {error}"
            )));
            return;
        }
    };
    let reserved = [
        "response_type",
        "client_id",
        "redirect_uri",
        "scope",
        "state",
        "code_challenge",
        "code_challenge_method",
    ];
    let preserved = authorization_url
        .query_pairs()
        .filter(|(name, _)| !reserved.contains(&name.as_ref()))
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    authorization_url.set_query(None);
    authorization_url.query_pairs_mut().extend_pairs(preserved);
    authorization_url
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &configuration.client_id)
        .append_pair("redirect_uri", &configuration.redirect_uri)
        .append_pair("scope", &configuration.required_scope)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    if tx
        .send(OAuthProgress::AuthorizationUrl(
            authorization_url.to_string(),
        ))
        .is_err()
    {
        return;
    }

    let callback = tokio::time::timeout(
        Duration::from_secs(300),
        receive_callback(&listener, &redirect, &state),
    )
    .await;
    let code = match callback {
        Ok(Ok(code)) => code,
        Ok(Err(error)) => {
            let _ = tx.send(OAuthProgress::Error(error));
            return;
        }
        Err(_) => {
            let _ = tx.send(OAuthProgress::Expired);
            return;
        }
    };
    match exchange_authorization_code(
        client,
        &metadata.token_endpoint,
        configuration,
        &code,
        &verifier,
    )
    .await
    {
        Ok(credential) => {
            let _ = tx.send(OAuthProgress::Credential(credential));
        }
        Err(error) => {
            let _ = tx.send(OAuthProgress::Error(error));
        }
    }
}

fn random_urlsafe(bytes: usize) -> String {
    let mut random = vec![0_u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut random);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random)
}

async fn receive_callback(
    listener: &TcpListener,
    redirect: &Url,
    expected_state: &str,
) -> Result<String, String> {
    loop {
        let (mut stream, _) = listener
            .accept()
            .await
            .map_err(|error| format!("accept OAuth callback: {error}"))?;
        let mut request = vec![0_u8; 16 * 1024];
        let read = stream
            .read(&mut request)
            .await
            .map_err(|error| format!("read OAuth callback: {error}"))?;
        let first_line = String::from_utf8_lossy(&request[..read])
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned();
        let expected_host = format!(
            "127.0.0.1:{}",
            redirect.port().expect("redirect port was validated")
        );
        let host_matches = String::from_utf8_lossy(&request[..read])
            .lines()
            .filter_map(|line| line.split_once(':'))
            .any(|(name, value)| {
                name.eq_ignore_ascii_case("host") && value.trim() == expected_host
            });
        if !host_matches {
            write_callback_response(&mut stream, 400, "Invalid OAuth callback host.").await;
            continue;
        }
        let target = first_line
            .strip_prefix("GET ")
            .and_then(|line| line.split_once(' ').map(|(target, _)| target));
        let Some(target) = target else {
            write_callback_response(&mut stream, 400, "Invalid OAuth callback.").await;
            continue;
        };
        let callback = Url::parse(&format!("http://127.0.0.1{target}"))
            .map_err(|error| format!("parse OAuth callback: {error}"))?;
        if callback.path() != redirect.path() {
            write_callback_response(&mut stream, 404, "Unknown callback path.").await;
            continue;
        }
        let parameters = callback
            .query_pairs()
            .into_owned()
            .collect::<std::collections::HashMap<_, _>>();
        if parameters.get("state").map(String::as_str) != Some(expected_state) {
            write_callback_response(&mut stream, 400, "OAuth state did not match.").await;
            return Err("the OAuth callback state did not match".to_owned());
        }
        if let Some(error) = parameters.get("error") {
            write_callback_response(&mut stream, 400, "Sign-in was not completed.").await;
            return Err(format!("authorization server returned `{error}`"));
        }
        let Some(code) = parameters.get("code").cloned() else {
            write_callback_response(&mut stream, 400, "The callback had no code.").await;
            return Err("the OAuth callback did not contain an authorization code".to_owned());
        };
        write_callback_response(
            &mut stream,
            200,
            "Sign-in complete. You can close this tab and return to Auru PM.",
        )
        .await;
        return Ok(code);
    }
}

async fn write_callback_response(stream: &mut tokio::net::TcpStream, status: u16, message: &str) {
    let status_text = if status == 200 { "OK" } else { "Bad Request" };
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>Auru PM sign-in</title><p>{message}</p>"
    );
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn exchange_authorization_code(
    client: &Client,
    token_endpoint: &str,
    configuration: &OAuthClientConfiguration,
    code: &str,
    verifier: &str,
) -> Result<ProviderCredential, String> {
    let response = client
        .post(token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", configuration.redirect_uri.as_str()),
            ("client_id", configuration.client_id.as_str()),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .map_err(|error| format!("exchange authorization code: {error}"))?;
    parse_token_response(response, token_endpoint, &configuration.client_id).await
}

async fn parse_token_response(
    response: reqwest::Response,
    token_endpoint: &str,
    client_id: &str,
) -> Result<ProviderCredential, String> {
    let status = response.status();
    let token: TokenResponse = response
        .json()
        .await
        .map_err(|error| format!("decode token response: {error}"))?;
    if !status.is_success() || token.access_token.is_none() {
        return Err(format!(
            "token endpoint rejected sign-in: {}",
            token
                .error_description
                .or(token.error)
                .unwrap_or_else(|| status.to_string())
        ));
    }
    if token
        .token_type
        .as_deref()
        .is_some_and(|token_type| !token_type.eq_ignore_ascii_case("bearer"))
    {
        return Err("token endpoint returned a non-bearer access token".to_owned());
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    Ok(ProviderCredential::OAuth {
        access_token: token.access_token.expect("checked above"),
        refresh_token: token.refresh_token,
        expires_at: token
            .expires_in
            .and_then(|seconds| i64::try_from(seconds).ok())
            .map(|seconds| now.saturating_add(seconds)),
        token_endpoint: token_endpoint.to_owned(),
        client_id: client_id.to_owned(),
        scope: token.scope,
    })
}

async fn run_discovered_device_flow(
    client: &Client,
    configuration: &OAuthClientConfiguration,
    metadata: &AuthorizationServerMetadata,
    tx: &mpsc::Sender<OAuthProgress>,
) {
    let Some(device_endpoint) = metadata.device_authorization_endpoint.as_deref() else {
        let _ = tx.send(OAuthProgress::Error(
            "the authorization server does not publish a device authorization endpoint".to_owned(),
        ));
        return;
    };
    let response = match client
        .post(device_endpoint)
        .form(&[
            ("client_id", configuration.client_id.as_str()),
            ("scope", configuration.required_scope.as_str()),
        ])
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            let _ = tx.send(OAuthProgress::Error(format!(
                "device authorization request failed: {error}"
            )));
            return;
        }
    };
    let device: DeviceCodeResponse = match response.json().await {
        Ok(device) => device,
        Err(error) => {
            let _ = tx.send(OAuthProgress::Error(format!(
                "device authorization response: {error}"
            )));
            return;
        }
    };
    let device_code = device.device_code.clone();
    let mut interval = device.interval.max(5);
    let deadline = std::time::Instant::now() + Duration::from_secs(device.expires_in);
    if tx.send(OAuthProgress::DeviceCode(device)).is_err() {
        return;
    }
    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        if std::time::Instant::now() >= deadline {
            let _ = tx.send(OAuthProgress::Expired);
            return;
        }
        let response = match client
            .post(&metadata.token_endpoint)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", &device_code),
                ("client_id", &configuration.client_id),
            ])
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                let _ = tx.send(OAuthProgress::Error(format!(
                    "device token request failed: {error}"
                )));
                return;
            }
        };
        if response.status().is_success() {
            match parse_token_response(response, &metadata.token_endpoint, &configuration.client_id)
                .await
            {
                Ok(credential) => {
                    let _ = tx.send(OAuthProgress::Credential(credential));
                }
                Err(error) => {
                    let _ = tx.send(OAuthProgress::Error(error));
                }
            }
            return;
        }
        let body: TokenResponse = match response.json().await {
            Ok(body) => body,
            Err(error) => {
                let _ = tx.send(OAuthProgress::Error(format!(
                    "device token response: {error}"
                )));
                return;
            }
        };
        match body.error.as_deref() {
            Some("authorization_pending") => {
                if tx.send(OAuthProgress::Pending).is_err() {
                    return;
                }
            }
            Some("slow_down") => {
                interval = interval.saturating_add(5);
                if tx.send(OAuthProgress::Pending).is_err() {
                    return;
                }
            }
            Some("access_denied") => {
                let _ = tx.send(OAuthProgress::AccessDenied);
                return;
            }
            Some("expired_token") => {
                let _ = tx.send(OAuthProgress::Expired);
                return;
            }
            error => {
                let _ = tx.send(OAuthProgress::Error(format!(
                    "device authorization failed: {}",
                    error.unwrap_or("unknown_error")
                )));
                return;
            }
        }
    }
}

/// Kick off the full device-code OAuth flow in a background thread.
///
/// The returned receiver streams [`OAuthProgress`] events:
/// - First a `DeviceCode` event so the UI can display the code.
/// - Periodic `Pending` events while polling.
/// - Finally `Token`, `Expired`, `AccessDenied`, or `Error`.
///
/// Drop the receiver to cancel the flow (the background thread detects the
/// broken channel and stops polling).
pub fn start_device_flow(base_url: String, client_id: String) -> mpsc::Receiver<OAuthProgress> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(run_device_flow(&base_url, &client_id, &tx));
    });
    rx
}

async fn run_device_flow(base_url: &str, client_id: &str, tx: &mpsc::Sender<OAuthProgress>) {
    let client = Client::new();
    let device_url = format!("{}/v1/auth/device/code", base_url.trim_end_matches('/'));
    let token_url = format!("{}/v1/auth/token", base_url.trim_end_matches('/'));

    // Step 1: request device code.
    let resp = match client
        .post(&device_url)
        .json(&DeviceCodeRequest { client_id })
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(OAuthProgress::Error(format!(
                "device code request failed: {e}"
            )));
            return;
        }
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let _ = tx.send(OAuthProgress::Error(format!(
            "device code error ({status}): {body}"
        )));
        return;
    }

    let device_code_resp: DeviceCodeResponse = match resp.json().await {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(OAuthProgress::Error(format!("device code parse: {e}")));
            return;
        }
    };

    let device_code = device_code_resp.device_code.clone();
    let interval = device_code_resp.interval;
    let expires_in = device_code_resp.expires_in;

    // Send code info to UI.
    if tx
        .send(OAuthProgress::DeviceCode(device_code_resp))
        .is_err()
    {
        return; // receiver dropped → cancelled
    }

    // Step 2: poll until done or expired.
    let deadline = std::time::Instant::now() + Duration::from_secs(expires_in);
    let mut poll_interval = Duration::from_secs(interval.max(5));

    loop {
        tokio::time::sleep(poll_interval).await;

        if std::time::Instant::now() >= deadline {
            let _ = tx.send(OAuthProgress::Expired);
            return;
        }

        let poll_resp = match client
            .post(&token_url)
            .json(&TokenPollRequest {
                grant_type: "urn:ietf:params:oauth:grant-type:device_code",
                device_code: &device_code,
                client_id,
            })
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(OAuthProgress::Error(format!("token poll error: {e}")));
                return;
            }
        };

        // RFC 8628 §3.5: 400 with error field is still a structured response.
        let body: TokenResponse = match poll_resp.json().await {
            Ok(b) => b,
            Err(e) => {
                let _ = tx.send(OAuthProgress::Error(format!("token parse error: {e}")));
                return;
            }
        };

        match (body.access_token, body.error.as_deref()) {
            (Some(token), _) => {
                let _ = tx.send(OAuthProgress::Token(token));
                return;
            }
            (_, Some("authorization_pending")) => {
                if tx.send(OAuthProgress::Pending).is_err() {
                    return; // cancelled
                }
            }
            (_, Some("slow_down")) => {
                // RFC 8628 requires increasing every subsequent interval by
                // five seconds after `slow_down`.
                poll_interval = poll_interval.saturating_add(Duration::from_secs(5));
                if tx.send(OAuthProgress::Pending).is_err() {
                    return;
                }
            }
            (_, Some("expired_token")) => {
                let _ = tx.send(OAuthProgress::Expired);
                return;
            }
            (_, Some("access_denied")) => {
                let _ = tx.send(OAuthProgress::AccessDenied);
                return;
            }
            (_, Some(other)) => {
                let _ = tx.send(OAuthProgress::Error(format!("OAuth error: {other}")));
                return;
            }
            (None, None) => {
                let _ = tx.send(OAuthProgress::Error("empty token response".into()));
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::extract::{Form, State};
    use axum::routing::post;
    use axum::{Json, Router};

    use super::*;

    type ReceivedForm = Arc<Mutex<Option<HashMap<String, String>>>>;

    async fn token_endpoint(
        State(received): State<ReceivedForm>,
        Form(form): Form<HashMap<String, String>>,
    ) -> Json<serde_json::Value> {
        *received.lock().unwrap() = Some(form);
        Json(serde_json::json!({
            "access_token": "access-token",
            "refresh_token": "refresh-token",
            "expires_in": 3600,
            "scope": "openid"
        }))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn authorization_code_flow_should_use_pkce_and_verify_the_loopback_state() {
        let received = Arc::new(Mutex::new(None));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let idp_address = listener.local_addr().unwrap();
        let idp = Router::new()
            .route("/token", post(token_endpoint))
            .with_state(received.clone());
        tokio::spawn(async move {
            axum::serve(listener, idp).await.unwrap();
        });

        let callback_port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let configuration = OAuthClientConfiguration {
            issuer: "https://identity.example.com".to_owned(),
            audience: "auru-pm".to_owned(),
            client_id: "desktop-client".to_owned(),
            required_scope: "openid".to_owned(),
            redirect_uri: format!("http://127.0.0.1:{callback_port}/oauth/callback"),
            flows: vec![OAuthFlow::AuthorizationCodePkce],
        };
        let metadata = AuthorizationServerMetadata {
            issuer: configuration.issuer.clone(),
            authorization_endpoint: "https://identity.example.com/authorize".to_owned(),
            token_endpoint: format!("http://{idp_address}/token"),
            device_authorization_endpoint: None,
            code_challenge_methods_supported: vec!["S256".to_owned()],
            jwks_uri: None,
            introspection_endpoint: None,
        };
        let (tx, rx) = mpsc::channel();
        let task = tokio::spawn(async move {
            run_authorization_code_flow(&Client::new(), &configuration, &metadata, &tx).await;
        });

        let OAuthProgress::AuthorizationUrl(authorization_url) =
            rx.recv_timeout(Duration::from_secs(2)).unwrap()
        else {
            panic!("authorization URL");
        };
        let authorization_url = Url::parse(&authorization_url).unwrap();
        let query = authorization_url
            .query_pairs()
            .into_owned()
            .collect::<HashMap<_, _>>();
        assert_eq!(
            query.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(
            query["redirect_uri"],
            format!("http://127.0.0.1:{callback_port}/oauth/callback")
        );

        let callback = format!(
            "http://127.0.0.1:{callback_port}/oauth/callback?code=one-time-code&state={}",
            query["state"]
        );
        let response = Client::new().get(callback).send().await.unwrap();
        assert!(response.status().is_success());
        let OAuthProgress::Credential(ProviderCredential::OAuth {
            access_token,
            refresh_token,
            ..
        }) = rx.recv_timeout(Duration::from_secs(2)).unwrap()
        else {
            panic!("OAuth credential");
        };
        assert_eq!(access_token, "access-token");
        assert_eq!(refresh_token.as_deref(), Some("refresh-token"));
        task.await.unwrap();

        let form = received.lock().unwrap().clone().unwrap();
        let verifier = &form["code_verifier"];
        let expected_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(query["code_challenge"], expected_challenge);
        assert_eq!(form["code"], "one-time-code");
        assert_eq!(form["client_id"], "desktop-client");
    }

    #[test]
    fn oauth_metadata_url_should_follow_rfc_8414_for_issuer_paths() {
        assert_eq!(
            oauth_metadata_url("https://identity.example.com/tenant").unwrap(),
            "https://identity.example.com/.well-known/oauth-authorization-server/tenant"
        );
    }
}

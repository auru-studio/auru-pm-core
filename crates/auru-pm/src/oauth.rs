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

use reqwest::Client;
use serde::{Deserialize, Serialize};

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
    /// Device code received — UI should show `user_code` and `verification_uri`.
    DeviceCode(DeviceCodeResponse),
    /// Still waiting for the user to authenticate.
    Pending,
    /// Successfully authenticated; carries the bearer token.
    Token(String),
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
    let poll_interval = Duration::from_secs(interval.max(5));

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
                // Back off by an extra interval.
                tokio::time::sleep(poll_interval).await;
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

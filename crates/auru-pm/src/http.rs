//! HTTP provider client implementing [`ProjectProvider`] over `auru-pm-v1`.
//!
//! Construct a project-scoped provider via [`HttpProvider::open`], or connect
//! an [`HttpAccount`] once when listing and opening several projects.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use reqwest::{Client, RequestBuilder, Response, StatusCode, header};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::commit::{Commit, CommitId, CommitSummary, HistoryRange};
use crate::error::{Error, Result};
use crate::hash::ContentHash;
use crate::provider::{
    Capabilities, HeadAdvance, Member, PermSet, ProjectProfile, ProjectProvider, ProviderProject,
    RetentionReport, RetentionRoots, RetentionRule, UserId,
};
use crate::token_store::{ProviderCredential, load_provider_credential, store_provider_credential};
use auru_pm_protocol::{AuthenticatedIdentity, OAuthClientConfiguration};

/// Gzip `bytes`, or `None` if the encoder failed — in which case the caller
/// sends the body uncompressed rather than failing the upload.
fn gzip(bytes: &[u8]) -> Option<Vec<u8>> {
    use std::io::Write as _;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).ok()?;
    encoder.finish().ok()
}

// ── Wire types ───────────────────────────────────────────────────────────────

/// Public server metadata returned before authentication.
#[derive(Clone, Debug)]
pub struct ProviderHealth {
    pub provider_id: Option<String>,
    pub name: String,
    pub capabilities: Capabilities,
    pub authentication: Option<OAuthClientConfiguration>,
}

#[derive(Deserialize)]
struct HeadResponse {
    commit_id: Option<CommitId>,
}

#[derive(Serialize)]
struct AdvanceHeadRequest {
    from: Option<CommitId>,
    to: CommitId,
}

#[derive(Deserialize)]
struct ConflictBody {
    #[serde(default)]
    current: Option<CommitId>,
}

#[derive(Deserialize)]
struct PutCommitResponse {
    id: CommitId,
}

#[derive(Deserialize)]
struct HistoryResponse {
    commits: Vec<CommitSummary>,
}

#[derive(Serialize)]
struct HasBlobsRequest<'a> {
    hashes: &'a [ContentHash],
}

#[derive(Deserialize)]
struct HasBlobsResponse {
    present: Vec<bool>,
}

#[derive(Deserialize)]
struct MembersResponse {
    members: Vec<Member>,
}

type ProjectsResponse =
    auru_pm_protocol::ProjectsResponse<CommitId, crate::project_format::ProjectFormat>;

// ── Provider ─────────────────────────────────────────────────────────────────

/// One authenticated HTTP provider account with cached capabilities.
#[derive(Clone, Debug)]
pub struct HttpAccount {
    client: Client,
    base_url: String,
    caps: Capabilities,
    provider_id: Option<String>,
    authentication: Option<OAuthClientConfiguration>,
    authenticator: Option<Authenticator>,
}

impl HttpAccount {
    /// Verify protocol compatibility and establish an authenticated account.
    pub async fn connect(base_url: &str, token: Option<String>) -> Result<Self> {
        let base_url = base_url.trim_end_matches('/').to_owned();
        let health = HttpProvider::probe_health(&base_url).await?;
        let client = build_client()?;
        let authenticator = token.map(|access_token| {
            Authenticator::memory(ProviderCredential::Pat { access_token }, client.clone())
        });
        Ok(Self {
            client,
            base_url,
            caps: health.capabilities,
            provider_id: health.provider_id,
            authentication: health.authentication,
            authenticator,
        })
    }

    /// Connect using an account credential stored in the OS keychain.
    pub async fn connect_stored(base_url: &str, credential_id: &str) -> Result<Self> {
        let base_url = base_url.trim_end_matches('/').to_owned();
        let health = HttpProvider::probe_health(&base_url).await?;
        let client = build_client()?;
        let authenticator = load_provider_credential(credential_id)?
            .map(|credential| Authenticator::stored(credential_id, credential, client.clone()));
        Ok(Self {
            client,
            base_url,
            caps: health.capabilities,
            provider_id: health.provider_id,
            authentication: health.authentication,
            authenticator,
        })
    }

    async fn send(&self, request: RequestBuilder) -> Result<Response> {
        send_authenticated(request, self.authenticator.as_ref()).await
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    pub fn provider_id(&self) -> Option<&str> {
        self.provider_id.as_deref()
    }

    pub fn authentication(&self) -> Option<&OAuthClientConfiguration> {
        self.authentication.as_ref()
    }

    /// Return the identity the PM server derived from the bearer token.
    pub async fn identity(&self) -> Result<AuthenticatedIdentity> {
        let response = self
            .send(self.client.get(format!("{}/v1/me", self.base_url)))
            .await?;
        check_resp(response)
            .await?
            .json()
            .await
            .map_err(|error| Error::Other(format!("identity parse: {error}")))
    }

    /// List projects visible to the authenticated provider account.
    pub async fn list_projects(&self) -> Result<Vec<ProviderProject>> {
        if !self.caps.project_listing {
            return Err(Error::Unsupported("project listing"));
        }
        let url = format!("{}/v1/projects", self.base_url);
        let resp = self.send(self.client.get(&url)).await?;
        let resp = check_resp(resp).await?;
        let body: ProjectsResponse = resp
            .json()
            .await
            .map_err(|error| Error::Other(format!("list_projects parse: {error}")))?;
        Ok(body.projects)
    }

    /// Open a project through this account without probing the provider again.
    pub fn open_project(&self, handle: impl Into<String>) -> HttpProvider {
        HttpProvider {
            client: self.client.clone(),
            base_url: self.base_url.clone(),
            handle: handle.into(),
            caps: self.caps.clone(),
            authenticator: self.authenticator.clone(),
        }
    }
}

/// HTTP provider connecting to an `auru-pm-v1` server.
#[derive(Clone, Debug)]
pub struct HttpProvider {
    client: Client,
    /// Base URL, no trailing slash.
    base_url: String,
    /// Project handle (provider-scoped opaque path segment).
    handle: String,
    caps: Capabilities,
    authenticator: Option<Authenticator>,
}

impl HttpProvider {
    /// Fetch the complete public provider descriptor.
    pub async fn probe_health(base_url: &str) -> Result<ProviderHealth> {
        let base_url = base_url.trim_end_matches('/');
        let url = format!("{base_url}/v1/health");
        let resp = Client::new()
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Network(format!(
                "health check failed ({status}): {body}"
            )));
        }

        let health: auru_pm_protocol::HealthResponse<Capabilities> = resp
            .json()
            .await
            .map_err(|e| Error::Other(format!("health parse error: {e}")))?;

        if health.protocol != crate::WIRE_VERSION {
            return Err(Error::Other(format!(
                "protocol mismatch: server advertises {:?}, client expects {:?}",
                health.protocol,
                crate::WIRE_VERSION
            )));
        }

        Ok(ProviderHealth {
            provider_id: health.provider_id,
            name: health.name.unwrap_or_else(|| base_url.to_owned()),
            capabilities: health.capabilities,
            authentication: health.authentication,
        })
    }

    /// Probe `{base_url}/v1/health` — verify protocol compatibility and return
    /// the server name and capabilities. Used by the "Verify" button before
    /// the full `open` call.
    pub async fn probe(base_url: &str) -> Result<(String, Capabilities)> {
        let health = Self::probe_health(base_url).await?;
        Ok((health.name, health.capabilities))
    }

    /// Open an HTTP provider for `handle` at `base_url`.
    ///
    /// Calls `probe` to verify compatibility and cache capabilities.
    /// If the server requires a bearer token, supply it via `token`.
    pub async fn open(base_url: &str, handle: &str, token: Option<String>) -> Result<Self> {
        Ok(HttpAccount::connect(base_url, token)
            .await?
            .open_project(handle))
    }

    /// Open a project using the provider account credential from the keychain.
    pub async fn open_stored(base_url: &str, handle: &str, credential_id: &str) -> Result<Self> {
        Ok(HttpAccount::connect_stored(base_url, credential_id)
            .await?
            .open_project(handle))
    }

    /// Stable provider identifier. Used as key in sidecar `remotes` and
    /// `.auru` `known_providers`. Equals the base URL.
    pub fn provider_id(&self) -> &str {
        &self.base_url
    }

    /// Project handle (provider-scoped path segment).
    pub fn handle(&self) -> &str {
        &self.handle
    }

    fn project_root_url(&self) -> String {
        format!(
            "{}/v1/projects/{}",
            self.base_url,
            encode_path_segment(&self.handle)
        )
    }

    fn project_url(&self, suffix: &str) -> String {
        format!(
            "{}/{}",
            self.project_root_url(),
            suffix.trim_start_matches('/')
        )
    }

    async fn send(&self, request: RequestBuilder) -> Result<Response> {
        send_authenticated(request, self.authenticator.as_ref()).await
    }
}

fn encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn build_client() -> Result<Client> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| Error::Network(e.to_string()))
}

#[derive(Clone)]
struct Authenticator {
    credential: Arc<Mutex<ProviderCredential>>,
    storage_id: Option<String>,
    client: Client,
}

impl fmt::Debug for Authenticator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Authenticator")
            .field("storage_id", &self.storage_id)
            .field("credential", &"<redacted>")
            .finish()
    }
}

impl Authenticator {
    fn memory(credential: ProviderCredential, client: Client) -> Self {
        Self {
            credential: Arc::new(Mutex::new(credential)),
            storage_id: None,
            client,
        }
    }

    fn stored(storage_id: &str, credential: ProviderCredential, client: Client) -> Self {
        Self {
            credential: shared_stored_credential(storage_id, credential),
            storage_id: Some(storage_id.to_owned()),
            client,
        }
    }

    async fn access_token(&self, force_refresh: bool) -> Result<String> {
        let mut credential = self.credential.lock().await;
        let needs_refresh = match &*credential {
            ProviderCredential::Pat { .. } => false,
            ProviderCredential::OAuth { expires_at, .. } => {
                force_refresh
                    || expires_at.is_some_and(|expiry| expiry <= unix_time().saturating_add(60))
            }
        };
        if needs_refresh {
            *credential = refresh_credential(&self.client, &credential).await?;
            if let Some(storage_id) = &self.storage_id {
                store_provider_credential(storage_id, &credential)?;
            }
        }
        Ok(credential.access_token().to_owned())
    }

    async fn refresh_after_unauthorized(
        &self,
        rejected_access_token: &str,
    ) -> Result<Option<String>> {
        let mut credential = self.credential.lock().await;
        if matches!(*credential, ProviderCredential::Pat { .. }) {
            return Ok(None);
        }
        // A concurrent request may already have rotated the refresh token
        // while this request was in flight. Reuse that result instead of
        // immediately rotating a second time.
        if credential.access_token() != rejected_access_token {
            return Ok(Some(credential.access_token().to_owned()));
        }
        *credential = refresh_credential(&self.client, &credential).await?;
        if let Some(storage_id) = &self.storage_id {
            store_provider_credential(storage_id, &credential)?;
        }
        Ok(Some(credential.access_token().to_owned()))
    }
}

fn shared_stored_credential(
    storage_id: &str,
    loaded: ProviderCredential,
) -> Arc<Mutex<ProviderCredential>> {
    static SESSIONS: OnceLock<std::sync::Mutex<HashMap<String, Weak<Mutex<ProviderCredential>>>>> =
        OnceLock::new();
    let sessions = SESSIONS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut sessions = sessions.lock().unwrap();
    if let Some(credential) = sessions.get(storage_id).and_then(Weak::upgrade) {
        return credential;
    }
    sessions.retain(|_, credential| credential.strong_count() > 0);
    let credential = Arc::new(Mutex::new(loaded));
    sessions.insert(storage_id.to_owned(), Arc::downgrade(&credential));
    credential
}

async fn send_authenticated(
    request: RequestBuilder,
    authenticator: Option<&Authenticator>,
) -> Result<Response> {
    let Some(authenticator) = authenticator else {
        return request
            .send()
            .await
            .map_err(|error| Error::Network(error.to_string()));
    };
    let retry = request.try_clone();
    let access_token = authenticator.access_token(false).await?;
    let response = request
        .bearer_auth(&access_token)
        .send()
        .await
        .map_err(|error| Error::Network(error.to_string()))?;
    if response.status() != StatusCode::UNAUTHORIZED {
        return Ok(response);
    }
    let Some(retry) = retry else {
        return Ok(response);
    };
    let Some(access_token) = authenticator
        .refresh_after_unauthorized(&access_token)
        .await?
    else {
        return Ok(response);
    };
    retry
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|error| Error::Network(error.to_string()))
}

#[derive(Deserialize)]
struct RefreshTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
}

async fn refresh_credential(
    client: &Client,
    credential: &ProviderCredential,
) -> Result<ProviderCredential> {
    let ProviderCredential::OAuth {
        refresh_token: Some(refresh_token),
        token_endpoint,
        client_id,
        scope,
        ..
    } = credential
    else {
        return Err(Error::Auth(
            "the OAuth access token expired and no refresh token is available".to_owned(),
        ));
    };
    let response = client
        .post(token_endpoint)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", client_id.as_str()),
        ])
        .send()
        .await
        .map_err(|error| Error::Network(format!("refresh OAuth token: {error}")))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(Error::Auth(format!(
            "OAuth token refresh failed ({status}): {body}"
        )));
    }
    let refreshed: RefreshTokenResponse = response
        .json()
        .await
        .map_err(|error| Error::Other(format!("refresh token parse: {error}")))?;
    if refreshed
        .token_type
        .as_deref()
        .is_some_and(|token_type| !token_type.eq_ignore_ascii_case("bearer"))
    {
        return Err(Error::Auth(
            "OAuth token refresh returned a non-bearer token".to_owned(),
        ));
    }
    let expires_at = refreshed
        .expires_in
        .and_then(|seconds| i64::try_from(seconds).ok())
        .map(|seconds| unix_time().saturating_add(seconds));
    Ok(ProviderCredential::OAuth {
        access_token: refreshed.access_token,
        refresh_token: refreshed
            .refresh_token
            .or_else(|| Some(refresh_token.clone())),
        expires_at,
        token_endpoint: token_endpoint.clone(),
        client_id: client_id.clone(),
        scope: refreshed.scope.or_else(|| scope.clone()),
    })
}

fn unix_time() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Map a non-success response to an `Error`. Returns `Ok(resp)` when the
/// status is 2xx; consumes the response on error.
async fn check_resp(resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(Error::Auth(body)),
        StatusCode::NOT_FOUND => Err(Error::NotFound(body)),
        StatusCode::CONFLICT => Err(Error::HeadConflict),
        StatusCode::UNPROCESSABLE_ENTITY => Err(Error::Unsupported("unsupported")),
        _ => Err(Error::Other(format!("HTTP {status}: {body}"))),
    }
}

// ── Trait impl ───────────────────────────────────────────────────────────────

#[async_trait]
impl ProjectProvider for HttpProvider {
    fn capabilities(&self) -> Capabilities {
        self.caps.clone()
    }

    /// Upload a blob, compressing the body when the server said it can decode it.
    ///
    /// Snapshots are canonical JSON and compress about sevenfold, so this is
    /// most of the upload for a project of any size. The hash in the URL always
    /// names the *plaintext* — compression is purely a transfer encoding, and
    /// the server stores what it decodes.
    async fn put_blob(&self, hash: &ContentHash, bytes: &[u8]) -> Result<()> {
        let url = if self.caps.project_scoped_blobs {
            self.project_url(&format!("blobs/{hash}"))
        } else {
            format!("{}/v1/blobs/{hash}", self.base_url)
        };
        let mut request = self
            .client
            .put(&url)
            .header(header::CONTENT_TYPE, "application/octet-stream");

        // Only when the server advertised support. Compressing regardless
        // would corrupt blobs on any server that ignores the header.
        match self
            .caps
            .compressed_uploads
            .then(|| gzip(bytes))
            .and_then(|compressed| compressed.filter(|body| body.len() < bytes.len()))
        {
            Some(compressed) => {
                request = request
                    .header(header::CONTENT_ENCODING, "gzip")
                    .body(compressed);
            }
            None => request = request.body(bytes.to_vec()),
        }

        let resp = self.send(request).await?;
        check_resp(resp).await?;
        Ok(())
    }

    async fn has_blobs(&self, hashes: &[ContentHash]) -> Result<Vec<bool>> {
        let url = if self.caps.project_scoped_blobs {
            self.project_url("blobs/has")
        } else {
            format!("{}/v1/blobs/has", self.base_url)
        };
        let resp = self
            .send(self.client.post(&url).json(&HasBlobsRequest { hashes }))
            .await?;
        let resp = check_resp(resp).await?;
        let body: HasBlobsResponse = resp
            .json()
            .await
            .map_err(|e| Error::Other(format!("has_blobs parse: {e}")))?;
        Ok(body.present)
    }

    async fn get_blob(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        let url = if self.caps.project_scoped_blobs {
            self.project_url(&format!("blobs/{hash}"))
        } else {
            format!("{}/v1/blobs/{hash}", self.base_url)
        };
        let resp = self.send(self.client.get(&url)).await?;
        let resp = check_resp(resp).await?;
        let bytes = resp
            .bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| Error::Network(e.to_string()))?;
        let actual = ContentHash::of(&bytes);
        if actual != *hash {
            return Err(Error::Other(format!(
                "blob hash mismatch: requested {hash}, received {actual}"
            )));
        }
        Ok(bytes)
    }

    async fn put_commit(&self, commit: &Commit) -> Result<CommitId> {
        let url = self.project_url("commits");
        let resp = self.send(self.client.post(&url).json(commit)).await?;
        let resp = check_resp(resp).await?;
        let body: PutCommitResponse = resp
            .json()
            .await
            .map_err(|e| Error::Other(format!("put_commit parse: {e}")))?;
        Ok(body.id)
    }

    async fn get_commit(&self, id: &CommitId) -> Result<Commit> {
        let url = self.project_url(&format!("commits/{}", id.0));
        let resp = self.send(self.client.get(&url)).await?;
        let resp = check_resp(resp).await?;
        resp.json()
            .await
            .map_err(|e| Error::Other(format!("get_commit parse: {e}")))
    }

    async fn list_history(&self, range: HistoryRange) -> Result<Vec<CommitSummary>> {
        let mut url = self.project_url("history");
        let mut params: Vec<String> = Vec::new();
        if let Some(limit) = range.limit {
            params.push(format!("limit={limit}"));
        }
        if let Some(before) = range.before {
            params.push(format!("before={}", before.0));
        }
        if !params.is_empty() {
            url.push('?');
            url.push_str(&params.join("&"));
        }
        let resp = self.send(self.client.get(&url)).await?;
        let resp = check_resp(resp).await?;
        let body: HistoryResponse = resp
            .json()
            .await
            .map_err(|e| Error::Other(format!("list_history parse: {e}")))?;
        Ok(body.commits)
    }

    async fn get_head(&self) -> Result<Option<CommitId>> {
        let url = self.project_url("head");
        let resp = self.send(self.client.get(&url)).await?;
        let resp = check_resp(resp).await?;
        let body: HeadResponse = resp
            .json()
            .await
            .map_err(|e| Error::Other(format!("get_head parse: {e}")))?;
        Ok(body.commit_id)
    }

    async fn advance_head(&self, from: Option<CommitId>, to: CommitId) -> Result<HeadAdvance> {
        let url = self.project_url("head");
        let resp = self
            .send(
                self.client
                    .post(&url)
                    .json(&AdvanceHeadRequest { from, to }),
            )
            .await?;

        // Handle 409 specially — carry back the server's current HEAD
        // without going through check_resp (which would return Error::HeadConflict).
        if resp.status() == StatusCode::CONFLICT {
            let body: ConflictBody = resp.json().await.unwrap_or(ConflictBody { current: None });
            return Ok(HeadAdvance::Conflict {
                current: body.current,
            });
        }

        check_resp(resp).await?;
        Ok(HeadAdvance::Advanced)
    }

    async fn put_project_profile(&self, profile: &ProjectProfile) -> Result<()> {
        if !self.caps.project_listing {
            return Err(Error::Unsupported("project listing"));
        }
        let resp = self
            .send(self.client.put(self.project_root_url()).json(profile))
            .await?;
        check_resp(resp).await?;
        Ok(())
    }

    async fn prune_history(
        &self,
        rule: RetentionRule,
        protected: &RetentionRoots,
    ) -> Result<RetentionReport> {
        if !self.caps.history_retention {
            return Err(Error::Unsupported("history retention"));
        }
        let request = auru_pm_protocol::RetentionRequest {
            rule,
            protected_commits: protected.commits.clone(),
            protected_blobs: protected.blobs.clone(),
        };
        let resp = self
            .send(
                self.client
                    .post(self.project_url("retention"))
                    .json(&request),
            )
            .await?;
        let resp = check_resp(resp).await?;
        resp.json()
            .await
            .map_err(|error| Error::Other(format!("prune_history parse: {error}")))
    }

    async fn list_members(&self) -> Result<Vec<Member>> {
        if !self.caps.members {
            return Err(Error::Unsupported("members"));
        }
        let url = self.project_url("members");
        let resp = self.send(self.client.get(&url)).await?;
        let resp = check_resp(resp).await?;
        let body: MembersResponse = resp
            .json()
            .await
            .map_err(|e| Error::Other(format!("list_members parse: {e}")))?;
        Ok(body.members)
    }

    async fn permissions(&self, user: &UserId) -> Result<PermSet> {
        if !self.caps.permissions {
            return Err(Error::Unsupported("permissions"));
        }
        let url = self.project_url(&format!("permissions/{user}"));
        let resp = self.send(self.client.get(&url)).await?;
        let resp = check_resp(resp).await?;
        resp.json()
            .await
            .map_err(|e| Error::Other(format!("permissions parse: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Bytes;
    use axum::extract::{Form, Path, State};
    use axum::http::HeaderMap;
    use axum::routing::{get, post};
    use axum::{Json, Router};

    use super::*;

    #[test]
    fn project_handle_is_encoded_as_one_path_segment() {
        assert_eq!(encode_path_segment("team/song one"), "team%2Fsong%20one");
    }

    #[derive(Clone)]
    struct RefreshTestState {
        resource_requests: Arc<AtomicUsize>,
        refresh_requests: Arc<AtomicUsize>,
    }

    async fn protected_resource(
        State(state): State<RefreshTestState>,
        headers: HeaderMap,
    ) -> StatusCode {
        state.resource_requests.fetch_add(1, Ordering::SeqCst);
        if headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            == Some("Bearer refreshed-access")
        {
            StatusCode::OK
        } else {
            StatusCode::UNAUTHORIZED
        }
    }

    async fn refresh_token(
        State(state): State<RefreshTestState>,
        Form(form): Form<HashMap<String, String>>,
    ) -> Json<serde_json::Value> {
        state.refresh_requests.fetch_add(1, Ordering::SeqCst);
        assert_eq!(form["grant_type"], "refresh_token");
        assert_eq!(form["refresh_token"], "original-refresh");
        Json(serde_json::json!({
            "access_token": "refreshed-access",
            "refresh_token": "rotated-refresh",
            "expires_in": 3600
        }))
    }

    #[tokio::test]
    async fn a_401_should_refresh_rotate_and_retry_an_oauth_credential_once() {
        let state = RefreshTestState {
            resource_requests: Arc::new(AtomicUsize::new(0)),
            refresh_requests: Arc::new(AtomicUsize::new(0)),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/resource", get(protected_resource))
            .route("/token", post(refresh_token))
            .with_state(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = Client::new();
        let authenticator = Authenticator::memory(
            ProviderCredential::OAuth {
                access_token: "stale-access".to_owned(),
                refresh_token: Some("original-refresh".to_owned()),
                expires_at: Some(unix_time() + 3600),
                token_endpoint: format!("http://{address}/token"),
                client_id: "desktop".to_owned(),
                scope: Some("openid".to_owned()),
            },
            client.clone(),
        );

        let response = send_authenticated(
            client.get(format!("http://{address}/resource")),
            Some(&authenticator),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(state.resource_requests.load(Ordering::SeqCst), 2);
        assert_eq!(state.refresh_requests.load(Ordering::SeqCst), 1);
        let credential = authenticator.credential.lock().await.clone();
        assert_eq!(credential.access_token(), "refreshed-access");
        let ProviderCredential::OAuth { refresh_token, .. } = credential else {
            panic!("OAuth credential");
        };
        assert_eq!(refresh_token.as_deref(), Some("rotated-refresh"));
    }

    async fn scoped_health() -> Json<auru_pm_protocol::HealthResponse<Capabilities>> {
        Json(auru_pm_protocol::HealthResponse {
            protocol: crate::WIRE_VERSION.to_owned(),
            provider_id: Some("scoped-provider".to_owned()),
            name: Some("Scoped provider".to_owned()),
            capabilities: Capabilities {
                project_scoped_blobs: true,
                ..Capabilities::default()
            },
            authentication: None,
        })
    }

    async fn scoped_blob(
        State(uploads): State<Arc<AtomicUsize>>,
        Path((_handle, _hash)): Path<(String, String)>,
        _body: Bytes,
    ) -> StatusCode {
        uploads.fetch_add(1, Ordering::SeqCst);
        StatusCode::OK
    }

    #[tokio::test]
    async fn project_scoped_blob_capability_should_select_the_private_routes() {
        let uploads = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/health", get(scoped_health))
            .route(
                "/v1/projects/:handle/blobs/:hash",
                axum::routing::put(scoped_blob),
            )
            .with_state(uploads.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let provider = HttpProvider::open(&format!("http://{address}"), "private song", None)
            .await
            .unwrap();
        let bytes = b"scoped";
        provider
            .put_blob(&ContentHash::of(bytes), bytes)
            .await
            .unwrap();
        assert_eq!(uploads.load(Ordering::SeqCst), 1);
    }
}

//! HTTP provider client implementing [`ProjectProvider`] over `auru-pm-v1`.
//!
//! Construct a project-scoped provider via [`HttpProvider::open`], or connect
//! an [`HttpAccount`] once when listing and opening several projects.

use async_trait::async_trait;
use reqwest::{Client, StatusCode, header};
use serde::{Deserialize, Serialize};

use crate::commit::{Commit, CommitId, CommitSummary, HistoryRange};
use crate::error::{Error, Result};
use crate::hash::ContentHash;
use crate::provider::{
    Capabilities, HeadAdvance, Member, PermSet, ProjectProfile, ProjectProvider, ProviderProject,
    UserId,
};

/// Gzip `bytes`, or `None` if the encoder failed — in which case the caller
/// sends the body uncompressed rather than failing the upload.
fn gzip(bytes: &[u8]) -> Option<Vec<u8>> {
    use std::io::Write as _;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).ok()?;
    encoder.finish().ok()
}

// ── Wire types ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct HealthResponse {
    protocol: String,
    #[serde(default)]
    name: Option<String>,
    capabilities: Capabilities,
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
}

impl HttpAccount {
    /// Verify protocol compatibility and establish an authenticated account.
    pub async fn connect(base_url: &str, token: Option<String>) -> Result<Self> {
        let base_url = base_url.trim_end_matches('/').to_owned();
        let (_, caps) = HttpProvider::probe(&base_url).await?;
        let client = build_client(token.as_deref())?;
        Ok(Self {
            client,
            base_url,
            caps,
        })
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    /// List projects visible to the authenticated provider account.
    pub async fn list_projects(&self) -> Result<Vec<ProviderProject>> {
        if !self.caps.project_listing {
            return Err(Error::Unsupported("project listing"));
        }
        let url = format!("{}/v1/projects", self.base_url);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|error| Error::Network(error.to_string()))?;
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
}

impl HttpProvider {
    /// Probe `{base_url}/v1/health` — verify protocol compatibility and return
    /// the server name and capabilities. Used by the "Verify" button before
    /// the full `open` call.
    pub async fn probe(base_url: &str) -> Result<(String, Capabilities)> {
        let url = format!("{}/v1/health", base_url.trim_end_matches('/'));
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

        let health: HealthResponse = resp
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

        let name = health.name.unwrap_or_else(|| base_url.to_owned());
        Ok((name, health.capabilities))
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

fn build_client(token: Option<&str>) -> Result<Client> {
    let mut builder = Client::builder();
    if let Some(t) = token {
        let mut headers = header::HeaderMap::new();
        let value = header::HeaderValue::from_str(&format!("Bearer {t}"))
            .map_err(|e| Error::Other(format!("invalid token for Authorization header: {e}")))?;
        headers.insert(header::AUTHORIZATION, value);
        builder = builder.default_headers(headers);
    }
    builder.build().map_err(|e| Error::Network(e.to_string()))
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
        let url = format!("{}/v1/blobs/{}", self.base_url, hash);
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

        let resp = request
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;
        check_resp(resp).await?;
        Ok(())
    }

    async fn has_blobs(&self, hashes: &[ContentHash]) -> Result<Vec<bool>> {
        let url = format!("{}/v1/blobs/has", self.base_url);
        let resp = self
            .client
            .post(&url)
            .json(&HasBlobsRequest { hashes })
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;
        let resp = check_resp(resp).await?;
        let body: HasBlobsResponse = resp
            .json()
            .await
            .map_err(|e| Error::Other(format!("has_blobs parse: {e}")))?;
        Ok(body.present)
    }

    async fn get_blob(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        let url = format!("{}/v1/blobs/{}", self.base_url, hash);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;
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
        let resp = self
            .client
            .post(&url)
            .json(commit)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;
        let resp = check_resp(resp).await?;
        let body: PutCommitResponse = resp
            .json()
            .await
            .map_err(|e| Error::Other(format!("put_commit parse: {e}")))?;
        Ok(body.id)
    }

    async fn get_commit(&self, id: &CommitId) -> Result<Commit> {
        let url = self.project_url(&format!("commits/{}", id.0));
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;
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
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;
        let resp = check_resp(resp).await?;
        let body: HistoryResponse = resp
            .json()
            .await
            .map_err(|e| Error::Other(format!("list_history parse: {e}")))?;
        Ok(body.commits)
    }

    async fn get_head(&self) -> Result<Option<CommitId>> {
        let url = self.project_url("head");
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;
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
            .client
            .post(&url)
            .json(&AdvanceHeadRequest { from, to })
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;

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
            .client
            .put(self.project_root_url())
            .json(profile)
            .send()
            .await
            .map_err(|error| Error::Network(error.to_string()))?;
        check_resp(resp).await?;
        Ok(())
    }

    async fn list_members(&self) -> Result<Vec<Member>> {
        if !self.caps.members {
            return Err(Error::Unsupported("members"));
        }
        let url = self.project_url("members");
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;
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
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;
        let resp = check_resp(resp).await?;
        resp.json()
            .await
            .map_err(|e| Error::Other(format!("permissions parse: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::encode_path_segment;

    #[test]
    fn project_handle_is_encoded_as_one_path_segment() {
        assert_eq!(encode_path_segment("team/song one"), "team%2Fsong%20one");
    }
}

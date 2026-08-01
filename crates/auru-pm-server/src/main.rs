//! Persistent `auru-pm-v1` reference server for development and protocol tests.
//!
//! Usage:
//!   cargo run -p auru-pm-server -- --port 4242 --data-dir ./server-data
//!   cargo run -p auru-pm-server -- --config ./server.toml
//!
//! The no-config compatibility mode advertises `auth_methods: ["none"]` and
//! is loopback-only. Deployments use versioned TOML and standards-based OAuth.

use std::collections::{BTreeSet, HashMap};
use std::env;
use std::fs;
use std::io::{self, Write as _};
use std::net::IpAddr;
use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use atomic_write_file::AtomicWriteFile;
use auru_pm::{Commit, ContentHash, SampleManifest, compute_commit_id};
use auru_pm_protocol::{
    HealthResponse, OAuthClientConfiguration, ProjectProfile, ProjectsResponse, ProviderProject,
    RetentionReport, RetentionRequest, WIRE_VERSION,
};
use axum::body::Bytes;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tower_http::decompression::RequestDecompressionLayer;

mod auth;
mod config;

// ── Shared state ─────────────────────────────────────────────────────────────

#[derive(Clone, Default, Serialize, Deserialize)]
struct Db {
    #[serde(skip)]
    blobs: HashMap<String, Vec<u8>>,
    commits: HashMap<String, Value>,
    projects: HashMap<String, StoredProject>,
    #[serde(skip)]
    data_dir: Option<PathBuf>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct StoredProject {
    #[serde(default)]
    owner: Option<Principal>,
    #[serde(default)]
    handle: Option<String>,
    head: Option<String>,
    history_floor: Option<String>,
    profile: Option<ProjectProfile<String>>,
    updated_at: i64,
    #[serde(default)]
    commits: BTreeSet<String>,
    #[serde(default)]
    blobs: BTreeSet<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Principal {
    issuer: String,
    subject: String,
}

impl From<&auth::TokenIdentity> for Principal {
    fn from(identity: &auth::TokenIdentity) -> Self {
        Self {
            issuer: identity.issuer.clone(),
            subject: identity.subject.clone(),
        }
    }
}

type SharedDb = Arc<Mutex<Db>>;

impl Db {
    fn open(data_dir: impl Into<PathBuf>) -> io::Result<Self> {
        let data_dir = data_dir.into();
        fs::create_dir_all(data_dir.join("blobs"))?;
        let state_path = data_dir.join("state.json");
        let mut db = if state_path.exists() {
            serde_json::from_slice(&fs::read(&state_path)?)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        } else {
            Self::default()
        };
        db.data_dir = Some(data_dir);
        Ok(db)
    }

    fn persist(&self) -> io::Result<()> {
        let Some(data_dir) = &self.data_dir else {
            return Ok(());
        };
        let state_path = data_dir.join("state.json");
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        let mut file = AtomicWriteFile::open(state_path)?;
        file.write_all(&bytes)?;
        file.commit()
    }

    /// Apply a state change only after its durable representation succeeds.
    ///
    /// Disk-backed state is staged in a clone, persisted atomically, and then
    /// swapped into the live database. In-memory test/dev state can mutate in
    /// place because there is no persistence operation to fail.
    fn mutate_persisted<T>(&mut self, mutation: impl FnOnce(&mut Self) -> T) -> io::Result<T> {
        if self.data_dir.is_none() {
            return Ok(mutation(self));
        }
        let mut staged = self.clone();
        let output = mutation(&mut staged);
        staged.persist()?;
        *self = staged;
        Ok(output)
    }

    fn put_blob(&mut self, hash: &str, bytes: &[u8]) -> io::Result<()> {
        let Some(data_dir) = &self.data_dir else {
            self.blobs.insert(hash.to_owned(), bytes.to_vec());
            return Ok(());
        };
        let destination = blob_path(data_dir, hash);
        if destination.exists() {
            return Ok(());
        }
        let temporary = destination.with_extension("new");
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, destination)
    }

    fn get_blob(&self, hash: &str) -> io::Result<Option<Vec<u8>>> {
        let Some(data_dir) = &self.data_dir else {
            return Ok(self.blobs.get(hash).cloned());
        };
        let path = blob_path(data_dir, hash);
        match fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn has_blob(&self, hash: &str) -> io::Result<bool> {
        let Some(data_dir) = &self.data_dir else {
            return Ok(self.blobs.contains_key(hash));
        };
        Ok(blob_path(data_dir, hash).is_file())
    }

    fn prepare_ownership(
        &mut self,
        authentication: &config::AuthenticationConfig,
    ) -> Result<(), String> {
        let legacy_handles = self
            .projects
            .iter()
            .filter(|(_, project)| project.owner.is_none())
            .map(|(handle, _)| handle.clone())
            .collect::<Vec<_>>();
        if legacy_handles.is_empty() {
            return Ok(());
        }
        let owner = match authentication {
            config::AuthenticationConfig::None { .. } => Principal {
                issuer: "local".to_owned(),
                subject: "local-user".to_owned(),
            },
            config::AuthenticationConfig::OAuth(oauth) => Principal {
                issuer: oauth.issuer.clone(),
                subject: oauth.legacy_owner_subject.clone().ok_or_else(|| {
                    "existing projects have no owner; set authentication.legacy_owner_subject to explicitly claim them"
                        .to_owned()
                })?,
            },
        };
        for legacy_handle in &legacy_handles {
            let target = project_key(&owner, legacy_handle);
            if self.projects.contains_key(&target) {
                return Err(format!(
                    "cannot migrate legacy project `{legacy_handle}` because its owned destination already exists"
                ));
            }
        }

        let mut replacements = Vec::with_capacity(legacy_handles.len());
        for legacy_handle in legacy_handles {
            let mut project = self
                .projects
                .remove(&legacy_handle)
                .expect("legacy project was discovered above");
            project.owner = Some(owner.clone());
            project.handle = Some(legacy_handle.clone());
            collect_legacy_entitlements(self, &mut project);
            replacements.push((project_key(&owner, &legacy_handle), project));
        }
        for (key, project) in replacements {
            self.projects.insert(key, project);
        }
        self.persist()
            .map_err(|error| format!("persist project ownership migration: {error}"))
    }
}

fn collect_legacy_entitlements(db: &Db, project: &mut StoredProject) {
    let mut pending = project.head.clone().into_iter().collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        if !project.commits.insert(id.clone()) {
            continue;
        }
        let Some(commit) = db.commits.get(&id) else {
            continue;
        };
        for field in [
            commit.pointer("/tree/snapshot"),
            commit.pointer("/tree/samples"),
            commit.get("metadata"),
        ]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        {
            project.blobs.insert(field.to_owned());
        }
        pending.extend(
            commit
                .get("parents")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
    }

    let manifests = project
        .blobs
        .iter()
        .filter_map(|hash| db.get_blob(hash).ok().flatten())
        .filter_map(|bytes| serde_json::from_slice::<SampleManifest>(&bytes).ok())
        .flat_map(|manifest| {
            manifest
                .entries
                .into_iter()
                .map(|entry| entry.hash.to_string())
        })
        .collect::<Vec<_>>();
    project.blobs.extend(manifests);
}

fn project_key(owner: &Principal, handle: &str) -> String {
    format!(
        "{}:{}:{}:{}:{handle}",
        owner.issuer.len(),
        owner.issuer,
        owner.subject.len(),
        owner.subject
    )
}

fn project_for<'a>(
    db: &'a Db,
    identity: &auth::TokenIdentity,
    handle: &str,
) -> Option<&'a StoredProject> {
    db.projects.get(&project_key(&identity.into(), handle))
}

fn project_for_mut<'a>(
    db: &'a mut Db,
    identity: &auth::TokenIdentity,
    handle: &str,
) -> Option<&'a mut StoredProject> {
    db.projects.get_mut(&project_key(&identity.into(), handle))
}

fn blob_path(data_dir: &FsPath, hash: &str) -> PathBuf {
    let mut name = String::with_capacity(hash.len() * 2);
    for byte in hash.as_bytes() {
        use std::fmt::Write as _;
        let _ = write!(name, "{byte:02x}");
    }
    data_dir.join("blobs").join(name)
}

struct RateLimiter {
    requests_per_minute: u32,
    clients: Mutex<HashMap<IpAddr, RequestWindow>>,
}

#[derive(Clone, Copy)]
struct RequestWindow {
    started: Instant,
    requests: u32,
}

impl RateLimiter {
    fn new(requests_per_minute: u32) -> Self {
        Self {
            requests_per_minute: requests_per_minute.max(1),
            clients: Mutex::new(HashMap::new()),
        }
    }

    fn allow_at(&self, client: IpAddr, now: Instant) -> bool {
        let mut clients = self.clients.lock().unwrap();
        if clients.len() >= 4_096 {
            clients
                .retain(|_, window| now.duration_since(window.started) < Duration::from_secs(60));
        }
        let window = clients.entry(client).or_insert(RequestWindow {
            started: now,
            requests: 0,
        });
        if now.duration_since(window.started) >= Duration::from_secs(60) {
            *window = RequestWindow {
                started: now,
                requests: 0,
            };
        }
        if window.requests >= self.requests_per_minute {
            return false;
        }
        window.requests += 1;
        true
    }
}

// ── Error helpers ────────────────────────────────────────────────────────────

fn err(status: StatusCode, code: &str, msg: &str) -> Response {
    (status, Json(json!({"code": code, "message": msg}))).into_response()
}

fn not_found(msg: &str) -> Response {
    err(StatusCode::NOT_FOUND, "not_found", msg)
}

fn bad_req(msg: &str) -> Response {
    err(StatusCode::BAD_REQUEST, "bad_request", msg)
}

fn internal(msg: &str) -> Response {
    err(StatusCode::INTERNAL_SERVER_ERROR, "storage_error", msg)
}

fn conflict(current: Option<&str>) -> Response {
    let body = json!({"code": "head_conflict", "current": current});
    (StatusCode::CONFLICT, Json(body)).into_response()
}

async fn enforce_rate_limit(
    State(limiter): State<Arc<RateLimiter>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let client = request
        .extensions()
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map(|connect| connect.0.ip())
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    if !limiter.allow_at(client, Instant::now()) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, "60")],
            Json(json!({
                "code": "rate_limited",
                "message": "request limit exceeded; retry in at most 60 seconds"
            })),
        )
            .into_response();
    }
    next.run(request).await
}

// ── Handlers ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct HealthDocument(HealthResponse<Value>);

impl HealthDocument {
    fn from_config(config: &config::ServerConfig) -> Self {
        let (authentication, auth_methods) = match &config.authentication {
            config::AuthenticationConfig::None { .. } => (None, json!(["none"])),
            config::AuthenticationConfig::OAuth(oauth) => (
                Some(OAuthClientConfiguration {
                    issuer: oauth.issuer.clone(),
                    audience: oauth.audience.clone(),
                    client_id: oauth.desktop_client_id.clone(),
                    required_scope: oauth.required_scope.clone(),
                    redirect_uri: oauth.redirect_uri.clone(),
                    flows: oauth.flows.clone(),
                }),
                Value::Array(
                    oauth
                        .flows
                        .iter()
                        .map(|flow| match flow {
                            auru_pm_protocol::OAuthFlow::AuthorizationCodePkce => {
                                Value::String("authorization_code_pkce".to_owned())
                            }
                            auru_pm_protocol::OAuthFlow::DeviceAuthorization => {
                                Value::String("oauth_device_code".to_owned())
                            }
                        })
                        .collect(),
                ),
            ),
        };
        Self(HealthResponse {
            protocol: WIRE_VERSION.to_owned(),
            provider_id: Some(config.provider_id.clone()),
            name: Some("Auru PM server".to_owned()),
            capabilities: json!({
            "project_listing": true,
            "members": false,
            "permissions": false,
            "branches": false,
            "server_side_merge": false,
            "auth_methods": auth_methods,
            // Blob uploads may arrive gzipped; the decompression layer on the
            // router unwraps them before the handler sees the body.
            "compressed_uploads": true,
            "history_retention": true,
            "project_scoped_blobs": true
            }),
            authentication,
        })
    }
}

async fn get_health(Extension(health): Extension<HealthDocument>) -> impl IntoResponse {
    Json(health.0)
}

async fn get_me(
    Extension(identity): Extension<auru_pm_protocol::AuthenticatedIdentity>,
) -> impl IntoResponse {
    Json(identity)
}

async fn get_projects(
    State(db): State<SharedDb>,
    Extension(identity): Extension<auth::TokenIdentity>,
) -> impl IntoResponse {
    let db = db.lock().unwrap();
    let mut projects: Vec<ProviderProject<String, String>> = db
        .projects
        .values()
        .filter(|project| project.owner.as_ref() == Some(&Principal::from(&identity)))
        .filter_map(|project| {
            Some(ProviderProject {
                handle: project.handle.clone()?,
                head: project.head.clone()?,
                profile: project.profile.clone(),
                updated_at: project.updated_at,
            })
        })
        .collect();
    projects.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.handle.cmp(&right.handle))
    });
    Json(ProjectsResponse { projects })
}

async fn put_project_profile(
    State(db): State<SharedDb>,
    Extension(identity): Extension<auth::TokenIdentity>,
    Path(handle): Path<String>,
    Json(mut profile): Json<ProjectProfile<String>>,
) -> Response {
    let owner = Principal::from(&identity);
    let key = project_key(&owner, &handle);
    let mut db = db.lock().unwrap();
    match db.mutate_persisted(|db| {
        let project = db.projects.entry(key).or_insert_with(|| StoredProject {
            owner: Some(owner),
            handle: Some(handle),
            ..StoredProject::default()
        });
        if profile.location.is_none() {
            profile.location = project
                .profile
                .as_ref()
                .and_then(|existing| existing.location.clone());
        }
        project.profile = Some(profile);
    }) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal(&format!("persist project profile: {error}")),
    }
}

async fn get_head(
    State(db): State<SharedDb>,
    Extension(identity): Extension<auth::TokenIdentity>,
    Path(handle): Path<String>,
) -> impl IntoResponse {
    let db = db.lock().unwrap();
    let head = project_for(&db, &identity, &handle).and_then(|project| project.head.clone());
    Json(json!({ "commit_id": head }))
}

#[derive(Deserialize)]
struct AdvanceHeadBody {
    from: Option<String>,
    to: String,
}

async fn post_head(
    State(db): State<SharedDb>,
    Extension(identity): Extension<auth::TokenIdentity>,
    Path(handle): Path<String>,
    Json(body): Json<AdvanceHeadBody>,
) -> Response {
    let mut db = db.lock().unwrap();
    let Some(project) = project_for(&db, &identity, &handle) else {
        return not_found(&format!("project {handle}"));
    };
    let current = project.head.as_deref();
    if current != body.from.as_deref() {
        return conflict(current);
    }
    if !project.commits.contains(&body.to) {
        return bad_req("the target commit does not belong to this project");
    }
    let updated_at = db
        .commits
        .get(&body.to)
        .and_then(|commit| commit.get("timestamp"))
        .and_then(Value::as_i64)
        .unwrap_or_default();
    match db.mutate_persisted(|db| {
        let project =
            project_for_mut(db, &identity, &handle).expect("project was checked before mutation");
        project.head = Some(body.to);
        project.updated_at = updated_at;
    }) {
        Ok(()) => (StatusCode::OK, Json(json!({"result": "advanced"}))).into_response(),
        Err(error) => internal(&format!("persist project head: {error}")),
    }
}

async fn post_commit(
    State(db): State<SharedDb>,
    Extension(identity): Extension<auth::TokenIdentity>,
    Extension(authenticated): Extension<auru_pm_protocol::AuthenticatedIdentity>,
    Path(handle): Path<String>,
    Json(commit): Json<Commit>,
) -> Response {
    let computed = match compute_commit_id(&commit) {
        Ok(computed) => computed,
        Err(error) => return bad_req(&format!("canonicalize commit: {error}")),
    };
    if commit.id != computed {
        return bad_req("commit id does not match its canonical content");
    }
    if commit.author.provider_id != authenticated.provider_id
        || commit.author.provider_user_id != authenticated.user_id
        || commit.author.display_name != authenticated.display_name
        || commit
            .author
            .email
            .as_ref()
            .is_some_and(|email| Some(email) != authenticated.email.as_ref())
    {
        return err(
            StatusCode::FORBIDDEN,
            "author_identity_mismatch",
            "commit author must match the authenticated identity",
        );
    }
    let id = commit.id.0.to_string();
    let mut value = match serde_json::to_value(&commit) {
        Ok(value) => value,
        Err(error) => return bad_req(&format!("encode commit: {error}")),
    };
    // Strip the id before storing (canonical encoding — matches filesystem provider).
    if let Value::Object(ref mut map) = value {
        map.remove("id");
    }
    let mut db = db.lock().unwrap();
    let Some(project) = project_for(&db, &identity, &handle) else {
        return not_found(&format!("project {handle}"));
    };
    if commit
        .parents
        .iter()
        .any(|parent| !project.commits.contains(&parent.0.to_string()))
    {
        return bad_req("every parent commit must belong to this project");
    }
    let required_blobs = [
        Some(commit.tree.snapshot),
        Some(commit.tree.samples),
        commit.metadata,
    ]
    .into_iter()
    .flatten()
    .map(|hash| hash.to_string())
    .collect::<Vec<_>>();
    if required_blobs
        .iter()
        .any(|hash| !project.blobs.contains(hash))
    {
        return bad_req("the commit references a blob not uploaded to this project");
    }
    let manifest_hash = commit.tree.samples.to_string();
    let manifest_bytes = match db.get_blob(&manifest_hash) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return bad_req("the commit sample manifest is missing"),
        Err(error) => return internal(&format!("read sample manifest: {error}")),
    };
    let manifest: SampleManifest = match serde_json::from_slice(&manifest_bytes) {
        Ok(manifest) => manifest,
        Err(error) => return bad_req(&format!("invalid sample manifest: {error}")),
    };
    if manifest
        .entries
        .iter()
        .any(|entry| !project.blobs.contains(&entry.hash.to_string()))
    {
        return bad_req("the sample manifest references a blob not uploaded to this project");
    }
    match db.mutate_persisted(|db| {
        db.commits.insert(id.clone(), value);
        project_for_mut(db, &identity, &handle)
            .expect("project was checked before mutation")
            .commits
            .insert(id.clone());
    }) {
        Ok(()) => (StatusCode::OK, Json(json!({"id": id}))).into_response(),
        Err(error) => internal(&format!("persist commit: {error}")),
    }
}

async fn get_commit(
    State(db): State<SharedDb>,
    Extension(identity): Extension<auth::TokenIdentity>,
    Path((handle, id)): Path<(String, String)>,
) -> Response {
    let db = db.lock().unwrap();
    let Some(project) = project_for(&db, &identity, &handle) else {
        return not_found(&format!("project {handle}"));
    };
    if !project.commits.contains(&id) {
        return not_found(&format!("commit {id}"));
    }
    match db.commits.get(&id) {
        Some(value) => {
            let mut full = value.clone();
            if let Value::Object(ref mut map) = full {
                map.insert("id".into(), Value::String(id));
            }
            Json(full).into_response()
        }
        None => not_found(&format!("commit {id}")),
    }
}

#[derive(Deserialize, Default)]
struct HistoryQuery {
    limit: Option<u32>,
    before: Option<String>,
}

async fn get_history(
    State(db): State<SharedDb>,
    Extension(identity): Extension<auth::TokenIdentity>,
    Path(handle): Path<String>,
    Query(q): Query<HistoryQuery>,
) -> impl IntoResponse {
    let db = db.lock().unwrap();
    let limit = q.limit.unwrap_or(100) as usize;
    let mut out: Vec<Value> = Vec::new();
    let mut started = q.before.is_none();
    let project = project_for(&db, &identity, &handle);
    let mut cursor = project.and_then(|project| project.head.clone());
    let floor = project.and_then(|project| project.history_floor.clone());

    while let Some(id) = cursor {
        if !project.is_some_and(|project| project.commits.contains(&id)) {
            break;
        }
        let commit_val = match db.commits.get(&id) {
            Some(v) => v.clone(),
            None => break,
        };
        let mut full = commit_val.clone();
        if let Value::Object(ref mut map) = full {
            map.insert("id".into(), Value::String(id.clone()));
        }
        if started {
            // Build a CommitSummary (drop `tree` and other full-commit-only fields).
            let summary = json!({
                "id": full["id"],
                "parents": full["parents"],
                "author": full["author"],
                "timestamp": full["timestamp"],
                "message": full["message"],
                "description": full.get("description").cloned().unwrap_or(Value::String(String::new())),
            });
            out.push(summary);
            if out.len() >= limit {
                break;
            }
        } else if Some(&id) == q.before.as_ref() {
            started = true;
        }
        if Some(&id) == floor.as_ref() {
            break;
        }
        // Walk first parent.
        cursor = commit_val
            .get("parents")
            .and_then(|p| p.as_array())
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .map(str::to_owned);
    }

    Json(json!({ "commits": out }))
}

async fn post_retention(
    State(db): State<SharedDb>,
    Extension(identity): Extension<auth::TokenIdentity>,
    Path(handle): Path<String>,
    Json(request): Json<RetentionRequest<String, String>>,
) -> Response {
    let mut db = db.lock().unwrap();
    let Some(project) = project_for(&db, &identity, &handle) else {
        return not_found(&format!("project {handle}"));
    };
    let mut cursor = project.head.clone();
    let current_floor = project.history_floor.clone();
    let mut history = Vec::new();
    while let Some(id) = cursor {
        let Some(commit) = db.commits.get(&id) else {
            break;
        };
        if !project.commits.contains(&id) {
            break;
        }
        let timestamp = commit
            .get("timestamp")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        history.push((id.clone(), timestamp));
        if Some(&id) == current_floor.as_ref() {
            break;
        }
        cursor = commit
            .get("parents")
            .and_then(Value::as_array)
            .and_then(|parents| parents.first())
            .and_then(Value::as_str)
            .map(str::to_owned);
    }

    let retained_len = request
        .rule
        .retained_prefix_len(history.iter().map(|(_, timestamp)| *timestamp));
    let Some(new_floor) = retained_len
        .checked_sub(1)
        .and_then(|index| history.get(index))
        .map(|(id, _)| id.clone())
    else {
        return Json(RetentionReport::default()).into_response();
    };
    let versions_removed = history.len().saturating_sub(retained_len);
    if let Err(error) = db.mutate_persisted(|db| {
        project_for_mut(db, &identity, &handle)
            .expect("project was checked above")
            .history_floor = Some(new_floor);
    }) {
        return internal(&format!("persist retention boundary: {error}"));
    }
    Json(RetentionReport {
        versions_removed: versions_removed as u64,
        // The development server has a process-global CAS. A production
        // provider can reclaim objects asynchronously once no project needs
        // them; this conformance server only enforces the history boundary.
        objects_removed: 0,
        bytes_freed: 0,
    })
    .into_response()
}

#[derive(Deserialize)]
struct HasBlobsBody {
    hashes: Vec<String>,
}

async fn post_blobs_has(
    State(db): State<SharedDb>,
    Extension(identity): Extension<auth::TokenIdentity>,
    Path(handle): Path<String>,
    Json(body): Json<HasBlobsBody>,
) -> Response {
    let db = db.lock().unwrap();
    let Some(project) = project_for(&db, &identity, &handle) else {
        return not_found(&format!("project {handle}"));
    };
    let present = body
        .hashes
        .iter()
        .map(|hash| {
            if !project.blobs.contains(hash) {
                return Ok(false);
            }
            db.has_blob(hash)
        })
        .collect::<io::Result<Vec<_>>>();
    match present {
        Ok(present) => Json(json!({ "present": present })).into_response(),
        Err(error) => internal(&format!("check blob storage: {error}")),
    }
}

async fn put_blob(
    State(db): State<SharedDb>,
    Extension(identity): Extension<auth::TokenIdentity>,
    Path((handle, hash)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    let parsed = match hash.parse::<ContentHash>() {
        Ok(hash) => hash,
        Err(error) => return bad_req(&format!("invalid blob hash: {error}")),
    };
    if ContentHash::of(&body) != parsed {
        return bad_req("blob bytes do not match the requested content hash");
    }
    let mut db = db.lock().unwrap();
    if project_for(&db, &identity, &handle).is_none() {
        return not_found(&format!("project {handle}"));
    }
    if let Err(error) = db.put_blob(&hash, &body) {
        return internal(&format!("persist blob: {error}"));
    }
    match db.mutate_persisted(|db| {
        project_for_mut(db, &identity, &handle)
            .expect("project was checked before mutation")
            .blobs
            .insert(hash);
    }) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => internal(&format!("persist project blob reference: {error}")),
    }
}

async fn get_blob(
    State(db): State<SharedDb>,
    Extension(identity): Extension<auth::TokenIdentity>,
    Path((handle, hash)): Path<(String, String)>,
) -> Response {
    let db = db.lock().unwrap();
    let Some(project) = project_for(&db, &identity, &handle) else {
        return not_found(&format!("project {handle}"));
    };
    if !project.blobs.contains(&hash) {
        return not_found(&format!("blob {hash}"));
    }
    match db.get_blob(&hash) {
        Ok(Some(bytes)) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
            bytes,
        )
            .into_response(),
        Ok(None) => not_found(&format!("blob {hash}")),
        Err(error) => internal(&format!("read blob: {error}")),
    }
}

// ── Main ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
fn app_with_auth(db: SharedDb, requests_per_minute: u32, auth: auth::AuthState) -> Router {
    let config = config::ServerConfig::unauthenticated_legacy(
        SocketAddr::from(([127, 0, 0, 1], 4242)),
        PathBuf::from("auru-pm-server-data"),
        requests_per_minute,
    );
    app_with_auth_and_health(
        db,
        requests_per_minute,
        auth,
        HealthDocument::from_config(&config),
    )
}

fn app_with_auth_and_health(
    db: SharedDb,
    requests_per_minute: u32,
    auth: auth::AuthState,
    health: HealthDocument,
) -> Router {
    let limiter = Arc::new(RateLimiter::new(requests_per_minute));
    let protected = Router::new()
        .route("/v1/me", get(get_me))
        .route("/v1/projects", get(get_projects))
        .route("/v1/projects/:handle", put(put_project_profile))
        .route("/v1/projects/:handle/head", get(get_head).post(post_head))
        .route("/v1/projects/:handle/commits", post(post_commit))
        .route("/v1/projects/:handle/commits/:id", get(get_commit))
        .route("/v1/projects/:handle/history", get(get_history))
        .route("/v1/projects/:handle/retention", post(post_retention))
        .route("/v1/projects/:handle/blobs/has", post(post_blobs_has))
        .route(
            "/v1/projects/:handle/blobs/:hash",
            put(put_blob).get(get_blob),
        )
        .layer(middleware::from_fn_with_state(auth, auth::require_auth));
    Router::new()
        .route("/v1/health", get(get_health))
        .merge(protected)
        // Unwrap `Content-Encoding: gzip` request bodies before they reach a
        // handler, so `put_blob` always stores plaintext regardless of how the
        // client chose to send it. Advertised as `compressed_uploads` in
        // `/v1/health`; clients only compress when they have seen that.
        .layer(RequestDecompressionLayer::new().gzip(true))
        .layer(middleware::from_fn_with_state(limiter, enforce_rate_limit))
        .layer(Extension(health))
        .with_state(db)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    let port_override: Option<u16> = args
        .windows(2)
        .find(|w| w[0] == "--port")
        .and_then(|w| w[1].parse().ok());
    let data_dir_override = args
        .windows(2)
        .find(|window| window[0] == "--data-dir")
        .map(|window| PathBuf::from(&window[1]));
    let requests_per_minute_override = args
        .windows(2)
        .find(|window| window[0] == "--requests-per-minute")
        .and_then(|window| window[1].parse().ok());
    let config_path = args
        .windows(2)
        .find(|window| window[0] == "--config")
        .map(|window| PathBuf::from(&window[1]));
    let mut config = if let Some(path) = config_path {
        let source = fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!("read server configuration {}: {error}", path.display())
        });
        config::ServerConfig::from_toml(&source)
            .unwrap_or_else(|error| panic!("invalid server configuration: {error}"))
    } else {
        config::ServerConfig::unauthenticated_legacy(
            SocketAddr::from(([127, 0, 0, 1], port_override.unwrap_or(4242))),
            data_dir_override
                .clone()
                .or_else(|| env::var_os("AURU_PM_SERVER_DATA_DIR").map(PathBuf::from))
                .unwrap_or_else(|| PathBuf::from("auru-pm-server-data")),
            requests_per_minute_override.unwrap_or(600),
        )
    };
    if let Some(port) = port_override {
        config.listen.set_port(port);
    }
    if let Some(data_dir) = data_dir_override {
        config.data_dir = data_dir;
    }
    if let Some(requests_per_minute) = requests_per_minute_override {
        config.requests_per_minute = requests_per_minute;
    }
    config
        .validate()
        .unwrap_or_else(|error| panic!("invalid server configuration: {error}"));

    let mut database =
        Db::open(&config.data_dir).expect("server data directory should be writable");
    database
        .prepare_ownership(&config.authentication)
        .unwrap_or_else(|error| panic!("initialize project ownership: {error}"));
    let db: SharedDb = Arc::new(Mutex::new(database));
    let auth = auth::build_auth_state(&config.provider_id, &config.authentication)
        .await
        .unwrap_or_else(|error| panic!("initialize authentication: {error}"));
    let app = app_with_auth_and_health(
        db,
        config.requests_per_minute,
        auth,
        HealthDocument::from_config(&config),
    );

    println!(
        "auru-pm server listening on http://{}; data: {}; limit: {} requests/minute/client",
        config.listen,
        config.data_dir.display(),
        config.requests_per_minute
    );
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .expect("server address should be available");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .expect("HTTP server should run until shutdown");
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use async_trait::async_trait;
    use auru_pm_protocol::RetentionRule;
    use tower::ServiceExt as _;

    struct RejectAllTokens;

    #[async_trait]
    impl auth::TokenVerifier for RejectAllTokens {
        async fn verify(&self, _token: &str) -> Result<auth::TokenIdentity, auth::AuthError> {
            Err(auth::AuthError::InvalidToken)
        }
    }

    struct AcceptTestToken;

    #[async_trait]
    impl auth::TokenVerifier for AcceptTestToken {
        async fn verify(&self, token: &str) -> Result<auth::TokenIdentity, auth::AuthError> {
            let (subject, display_name, email) = match token {
                "valid-test-token" => ("user_123", "Alice Example", "alice@example.com"),
                "bob-test-token" => ("user_456", "Bob Example", "bob@example.com"),
                _ => return Err(auth::AuthError::InvalidToken),
            };
            Ok(auth::TokenIdentity {
                issuer: "https://identity.example.com".to_owned(),
                subject: subject.to_owned(),
                display_name: display_name.to_owned(),
                email: Some(email.to_owned()),
                scopes: BTreeSet::from(["openid".to_owned()]),
            })
        }
    }

    #[tokio::test]
    async fn oauth_server_should_reject_a_request_without_a_bearer_token() {
        let db: SharedDb = Arc::new(Mutex::new(Db::default()));
        let auth = auth::AuthState::oauth("studio-pm", Arc::new(RejectAllTokens));
        let response = app_with_auth(db, 600, auth)
            .oneshot(
                Request::builder()
                    .uri("/v1/me")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({
                "code": "unauthorized",
                "message": "a bearer token is required"
            })
        );
    }

    #[tokio::test]
    async fn health_should_publish_only_the_public_oauth_client_configuration() {
        let config = config::ServerConfig::from_toml(
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
[authentication.validation]
strategy = "jwt"
"#,
        )
        .unwrap();
        let db: SharedDb = Arc::new(Mutex::new(Db::default()));
        let auth = auth::AuthState::oauth("studio-pm", Arc::new(RejectAllTokens));
        let response =
            app_with_auth_and_health(db, 600, auth, HealthDocument::from_config(&config))
                .oneshot(
                    Request::builder()
                        .uri("/v1/health")
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let health: HealthResponse<Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(health.provider_id.as_deref(), Some("studio-pm"));
        assert_eq!(
            health.authentication.unwrap().redirect_uri,
            "http://127.0.0.1:43827/oauth/callback"
        );
        let rendered = String::from_utf8(body.to_vec()).unwrap();
        assert!(!rendered.contains("secret"));
    }

    #[tokio::test]
    async fn me_should_return_the_identity_verified_from_the_bearer_token() {
        let db: SharedDb = Arc::new(Mutex::new(Db::default()));
        let auth = auth::AuthState::oauth("studio-pm", Arc::new(AcceptTestToken));
        let response = app_with_auth(db, 600, auth)
            .oneshot(
                Request::builder()
                    .uri("/v1/me")
                    .header(header::AUTHORIZATION, "Bearer valid-test-token")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({
                "provider_id": "studio-pm",
                "user_id": "user_123",
                "display_name": "Alice Example",
                "email": "alice@example.com"
            })
        );
    }

    async fn send_json(
        app: &Router,
        method: axum::http::Method,
        uri: &str,
        token: &str,
        body: Value,
    ) -> Response {
        app.clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn project_blobs_should_be_private_to_the_authenticated_identity() {
        let db: SharedDb = Arc::new(Mutex::new(Db::default()));
        let auth = auth::AuthState::oauth("studio-pm", Arc::new(AcceptTestToken));
        let app = app_with_auth(db, 600, auth);
        let profile = json!({"display_name": "Song", "format": "auru"});
        let response = send_json(
            &app,
            axum::http::Method::PUT,
            "/v1/projects/song",
            "valid-test-token",
            profile.clone(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let bytes = b"private project bytes";
        let hash = ContentHash::of(bytes).to_string();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(axum::http::Method::PUT)
                    .uri(format!("/v1/projects/song/blobs/{hash}"))
                    .header(header::AUTHORIZATION, "Bearer valid-test-token")
                    .body(axum::body::Body::from(bytes.as_slice()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/projects/song/blobs/{hash}"))
                    .header(header::AUTHORIZATION, "Bearer bob-test-token")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        send_json(
            &app,
            axum::http::Method::PUT,
            "/v1/projects/song",
            "bob-test-token",
            profile,
        )
        .await;
        let response = send_json(
            &app,
            axum::http::Method::POST,
            "/v1/projects/song/blobs/has",
            "bob-test-token",
            json!({"hashes": [hash]}),
        )
        .await;
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({"present": [false]}),
            "physical CAS deduplication must not grant another identity access"
        );
    }

    #[tokio::test]
    async fn project_profiles_should_persist_metadata_and_library_location() {
        let db: SharedDb = Arc::new(Mutex::new(Db::default()));
        let auth = auth::AuthState::oauth("studio-pm", Arc::new(AcceptTestToken));
        let app = app_with_auth(db.clone(), 600, auth);
        let profile = json!({
            "display_name": "Night Drive",
            "format": "auru",
            "metadata": {
                "genre": "Drum & Bass",
                "tags": ["work in progress", "collab"]
            },
            "location": {
                "relative_path": "Auru/Projects/Night Drive.auru"
            }
        });

        let response = send_json(
            &app,
            axum::http::Method::PUT,
            "/v1/projects/night-drive",
            "valid-test-token",
            profile.clone(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let update_without_location = json!({
            "display_name": "Night Drive (Renamed)",
            "format": "auru",
            "metadata": {
                "genre": "Drum & Bass",
                "tags": ["work in progress", "collab"]
            }
        });
        let response = send_json(
            &app,
            axum::http::Method::PUT,
            "/v1/projects/night-drive",
            "valid-test-token",
            update_without_location,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let expected = json!({
            "display_name": "Night Drive (Renamed)",
            "format": "auru",
            "metadata": {
                "genre": "Drum & Bass",
                "tags": ["work in progress", "collab"]
            },
            "location": {
                "relative_path": "Auru/Projects/Night Drive.auru"
            }
        });

        let stored = db
            .lock()
            .unwrap()
            .projects
            .values()
            .next()
            .and_then(|project| project.profile.as_ref())
            .map(serde_json::to_value)
            .transpose()
            .expect("stored profile encoding");
        assert_eq!(stored, Some(expected));
    }

    async fn upload_project_blob(
        app: &Router,
        handle: &str,
        token: &str,
        bytes: &[u8],
    ) -> ContentHash {
        let hash = ContentHash::of(bytes);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(axum::http::Method::PUT)
                    .uri(format!("/v1/projects/{handle}/blobs/{hash}"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(axum::body::Body::from(bytes.to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        hash
    }

    #[tokio::test]
    async fn commit_author_and_content_should_be_bound_to_the_authenticated_project() {
        let db: SharedDb = Arc::new(Mutex::new(Db::default()));
        let auth = auth::AuthState::oauth("studio-pm", Arc::new(AcceptTestToken));
        let app = app_with_auth(db, 600, auth);
        send_json(
            &app,
            axum::http::Method::PUT,
            "/v1/projects/song",
            "valid-test-token",
            json!({"display_name": "Song", "format": "auru"}),
        )
        .await;
        let snapshot = upload_project_blob(&app, "song", "valid-test-token", b"{}").await;
        let manifest_bytes = SampleManifest::default().canonical_encoding().unwrap();
        let samples = upload_project_blob(&app, "song", "valid-test-token", &manifest_bytes).await;
        let mut commit = Commit {
            id: auru_pm::CommitId(ContentHash::ZERO),
            parents: Vec::new(),
            tree: auru_pm::TreeRef { snapshot, samples },
            author: auru_pm::AuthorIdentity {
                display_name: "Alice Example".to_owned(),
                provider_user_id: "user_456".to_owned(),
                provider_id: "studio-pm".to_owned(),
                email: Some("alice@example.com".to_owned()),
            },
            timestamp: 1_800_000_000,
            message: "First version".to_owned(),
            description: String::new(),
            auru_version: "0.1.0".to_owned(),
            format_version: 1,
            metadata: None,
        };
        commit.id = compute_commit_id(&commit).unwrap();
        let response = send_json(
            &app,
            axum::http::Method::POST,
            "/v1/projects/song/commits",
            "valid-test-token",
            serde_json::to_value(&commit).unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        commit.author.provider_user_id = "user_123".to_owned();
        commit.id = compute_commit_id(&commit).unwrap();
        let response = send_json(
            &app,
            axum::http::Method::POST,
            "/v1/projects/song/commits",
            "valid-test-token",
            serde_json::to_value(&commit).unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/projects/song/commits/{}", commit.id.0))
                    .header(header::AUTHORIZATION, "Bearer bob-test-token")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn oauth_startup_should_require_an_explicit_owner_for_legacy_projects() {
        let mut db = Db::default();
        db.projects
            .insert("legacy-song".to_owned(), StoredProject::default());
        let config = config::ServerConfig::from_toml(
            r#"
version = 1
public_base_url = "https://pm.example.com"
[authentication]
mode = "oauth"
issuer = "https://identity.example.com"
audience = "auru-pm"
desktop_client_id = "desktop"
redirect_uri = "http://127.0.0.1:43827/oauth/callback"
[authentication.validation]
strategy = "jwt"
"#,
        )
        .unwrap();

        let error = db
            .prepare_ownership(&config.authentication)
            .expect_err("legacy projects must never be assigned implicitly");
        assert!(error.contains("legacy_owner_subject"), "{error}");
    }

    #[test]
    fn state_and_blobs_should_survive_reopening_the_data_directory() {
        let temp = tempfile::tempdir().expect("temporary data directory");
        {
            let mut db = Db::open(temp.path()).expect("open database");
            db.commits
                .insert("commit".into(), json!({ "parents": [], "timestamp": 100 }));
            db.projects.insert(
                "song".into(),
                StoredProject {
                    head: Some("commit".into()),
                    ..StoredProject::default()
                },
            );
            db.put_blob("blake3:asset", b"durable bytes")
                .expect("put blob");
            db.persist().expect("persist database");
        }

        let reopened = Db::open(temp.path()).expect("reopen database");
        assert!(reopened.commits.contains_key("commit"));
        assert_eq!(
            reopened
                .projects
                .get("song")
                .and_then(|project| project.head.as_deref()),
            Some("commit")
        );
        assert_eq!(
            reopened.get_blob("blake3:asset").expect("get blob"),
            Some(b"durable bytes".to_vec())
        );
    }

    #[test]
    fn failed_persistence_should_not_change_live_or_durable_state() {
        let temp = tempfile::tempdir().expect("temporary data directory");
        let mut db = Db::open(temp.path()).expect("open database");
        db.projects.insert(
            "song".into(),
            StoredProject {
                head: Some("old".into()),
                ..StoredProject::default()
            },
        );
        db.persist().expect("persist initial state");
        db.data_dir = Some(temp.path().join("missing-parent"));

        db.mutate_persisted(|staged| {
            staged.projects.get_mut("song").unwrap().head = Some("new".into());
        })
        .expect_err("persistence into a missing directory must fail");

        assert_eq!(
            db.projects
                .get("song")
                .and_then(|project| project.head.as_deref()),
            Some("old")
        );
        let reopened = Db::open(temp.path()).expect("reopen durable state");
        assert_eq!(
            reopened
                .projects
                .get("song")
                .and_then(|project| project.head.as_deref()),
            Some("old")
        );
    }

    #[test]
    fn rate_limiter_should_reset_each_clients_budget_after_one_minute() {
        let limiter = RateLimiter::new(2);
        let client = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
        let start = Instant::now();

        assert!(limiter.allow_at(client, start));
        assert!(limiter.allow_at(client, start));
        assert!(!limiter.allow_at(client, start));
        assert!(limiter.allow_at(client, start + Duration::from_secs(60)));
    }

    #[tokio::test]
    async fn retention_should_move_the_project_history_floor() {
        let db: SharedDb = Arc::new(Mutex::new(Db::default()));
        let identity = auth::TokenIdentity {
            issuer: "local".to_owned(),
            subject: "local-user".to_owned(),
            display_name: "Local user".to_owned(),
            email: None,
            scopes: BTreeSet::new(),
        };
        let key = project_key(&Principal::from(&identity), "song");
        {
            let mut db = db.lock().unwrap();
            db.commits
                .insert("first".into(), json!({ "parents": [], "timestamp": 100 }));
            db.commits.insert(
                "second".into(),
                json!({ "parents": ["first"], "timestamp": 200 }),
            );
            db.commits.insert(
                "third".into(),
                json!({ "parents": ["second"], "timestamp": 300 }),
            );
            db.projects.insert(
                key.clone(),
                StoredProject {
                    owner: Some(Principal::from(&identity)),
                    handle: Some("song".to_owned()),
                    head: Some("third".into()),
                    commits: BTreeSet::from([
                        "first".to_owned(),
                        "second".to_owned(),
                        "third".to_owned(),
                    ]),
                    ..StoredProject::default()
                },
            );
        }

        let response = post_retention(
            State(db.clone()),
            Extension(identity),
            Path("song".into()),
            Json(RetentionRequest {
                rule: RetentionRule::Latest { count: 2 },
                protected_commits: Vec::new(),
                protected_blobs: Vec::new(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            db.lock()
                .unwrap()
                .projects
                .get(&key)
                .and_then(|project| project.history_floor.as_deref())
                .map(str::to_owned),
            Some("second".to_owned())
        );
    }
}

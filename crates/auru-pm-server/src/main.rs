//! Persistent `auru-pm-v1` reference server for development and protocol tests.
//!
//! Usage:
//!   cargo run -p auru-pm-server -- --port 4242 --data-dir ./server-data
//!
//! The server advertises `auth_methods: ["none"]`. It must not be exposed to
//! an untrusted network even though request rate limiting is enabled.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, Write as _};
use std::net::IpAddr;
use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use atomic_write_file::AtomicWriteFile;
use auru_pm_protocol::{
    ProjectProfile, ProjectsResponse, ProviderProject, RetentionReport, RetentionRequest,
    WIRE_VERSION,
};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tower_http::decompression::RequestDecompressionLayer;

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
    head: Option<String>,
    history_floor: Option<String>,
    profile: Option<ProjectProfile<String>>,
    updated_at: i64,
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

async fn get_health() -> impl IntoResponse {
    Json(json!({
        "protocol": WIRE_VERSION,
        "name": "Auru PM development server",
        "capabilities": {
            "project_listing": true,
            "members": false,
            "permissions": false,
            "branches": false,
            "server_side_merge": false,
            "auth_methods": ["none"],
            // Blob uploads may arrive gzipped; the decompression layer on the
            // router unwraps them before the handler sees the body.
            "compressed_uploads": true,
            "history_retention": true
        }
    }))
}

async fn get_projects(State(db): State<SharedDb>) -> impl IntoResponse {
    let db = db.lock().unwrap();
    let mut projects: Vec<ProviderProject<String, String>> = db
        .projects
        .iter()
        .filter_map(|(handle, project)| {
            Some(ProviderProject {
                handle: handle.clone(),
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
    Path(handle): Path<String>,
    Json(profile): Json<ProjectProfile<String>>,
) -> Response {
    let mut db = db.lock().unwrap();
    match db.mutate_persisted(|db| {
        db.projects.entry(handle).or_default().profile = Some(profile);
    }) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal(&format!("persist project profile: {error}")),
    }
}

async fn get_head(State(db): State<SharedDb>, Path(handle): Path<String>) -> impl IntoResponse {
    let db = db.lock().unwrap();
    let head = db
        .projects
        .get(&handle)
        .and_then(|project| project.head.clone());
    Json(json!({ "commit_id": head }))
}

#[derive(Deserialize)]
struct AdvanceHeadBody {
    from: Option<String>,
    to: String,
}

async fn post_head(
    State(db): State<SharedDb>,
    Path(handle): Path<String>,
    Json(body): Json<AdvanceHeadBody>,
) -> Response {
    let mut db = db.lock().unwrap();
    let current = db
        .projects
        .get(&handle)
        .and_then(|project| project.head.as_deref());
    if current != body.from.as_deref() {
        return conflict(current);
    }
    let updated_at = db
        .commits
        .get(&body.to)
        .and_then(|commit| commit.get("timestamp"))
        .and_then(Value::as_i64)
        .unwrap_or_default();
    match db.mutate_persisted(|db| {
        let project = db.projects.entry(handle).or_default();
        project.head = Some(body.to);
        project.updated_at = updated_at;
    }) {
        Ok(()) => (StatusCode::OK, Json(json!({"result": "advanced"}))).into_response(),
        Err(error) => internal(&format!("persist project head: {error}")),
    }
}

async fn post_commit(State(db): State<SharedDb>, Json(commit): Json<Value>) -> Response {
    let id = match commit.get("id").and_then(Value::as_str) {
        Some(id) => id.to_owned(),
        None => return bad_req("missing id field"),
    };
    let mut value = commit.clone();
    // Strip the id before storing (canonical encoding — matches filesystem provider).
    if let Value::Object(ref mut map) = value {
        map.remove("id");
    }
    let mut db = db.lock().unwrap();
    match db.mutate_persisted(|db| {
        db.commits.insert(id.clone(), value);
    }) {
        Ok(()) => (StatusCode::OK, Json(json!({"id": id}))).into_response(),
        Err(error) => internal(&format!("persist commit: {error}")),
    }
}

async fn get_commit(
    State(db): State<SharedDb>,
    Path((_handle, id)): Path<(String, String)>,
) -> Response {
    let db = db.lock().unwrap();
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
    Path(handle): Path<String>,
    Query(q): Query<HistoryQuery>,
) -> impl IntoResponse {
    let db = db.lock().unwrap();
    let limit = q.limit.unwrap_or(100) as usize;
    let mut out: Vec<Value> = Vec::new();
    let mut started = q.before.is_none();
    let mut cursor = db
        .projects
        .get(&handle)
        .and_then(|project| project.head.clone());
    let floor = db
        .projects
        .get(&handle)
        .and_then(|project| project.history_floor.clone());

    while let Some(id) = cursor {
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
    Path(handle): Path<String>,
    Json(request): Json<RetentionRequest<String, String>>,
) -> Response {
    let mut db = db.lock().unwrap();
    let Some(project) = db.projects.get(&handle) else {
        return not_found(&format!("project {handle}"));
    };
    let mut cursor = project.head.clone();
    let current_floor = project.history_floor.clone();
    let mut history = Vec::new();
    while let Some(id) = cursor {
        let Some(commit) = db.commits.get(&id) else {
            break;
        };
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
        db.projects
            .get_mut(&handle)
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

async fn post_blobs_has(State(db): State<SharedDb>, Json(body): Json<HasBlobsBody>) -> Response {
    let db = db.lock().unwrap();
    let present = body
        .hashes
        .iter()
        .map(|hash| db.has_blob(hash))
        .collect::<io::Result<Vec<_>>>();
    match present {
        Ok(present) => Json(json!({ "present": present })).into_response(),
        Err(error) => internal(&format!("check blob storage: {error}")),
    }
}

async fn put_blob(State(db): State<SharedDb>, Path(hash): Path<String>, body: Bytes) -> Response {
    match db.lock().unwrap().put_blob(&hash, &body) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => internal(&format!("persist blob: {error}")),
    }
}

async fn get_blob(State(db): State<SharedDb>, Path(hash): Path<String>) -> Response {
    match db.lock().unwrap().get_blob(&hash) {
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

fn app(db: SharedDb, requests_per_minute: u32) -> Router {
    let limiter = Arc::new(RateLimiter::new(requests_per_minute));
    Router::new()
        .route("/v1/health", get(get_health))
        .route("/v1/projects", get(get_projects))
        .route("/v1/projects/:handle", put(put_project_profile))
        .route("/v1/projects/:handle/head", get(get_head).post(post_head))
        .route("/v1/projects/:handle/commits", post(post_commit))
        .route("/v1/projects/:handle/commits/:id", get(get_commit))
        .route("/v1/projects/:handle/history", get(get_history))
        .route("/v1/projects/:handle/retention", post(post_retention))
        .route("/v1/blobs/has", post(post_blobs_has))
        .route("/v1/blobs/:hash", put(put_blob).get(get_blob))
        // Unwrap `Content-Encoding: gzip` request bodies before they reach a
        // handler, so `put_blob` always stores plaintext regardless of how the
        // client chose to send it. Advertised as `compressed_uploads` in
        // `/v1/health`; clients only compress when they have seen that.
        .layer(RequestDecompressionLayer::new().gzip(true))
        .layer(middleware::from_fn_with_state(limiter, enforce_rate_limit))
        .with_state(db)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    let port: u16 = args
        .windows(2)
        .find(|w| w[0] == "--port")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(4242);
    let data_dir = args
        .windows(2)
        .find(|window| window[0] == "--data-dir")
        .map(|window| PathBuf::from(&window[1]))
        .or_else(|| env::var_os("AURU_PM_SERVER_DATA_DIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("auru-pm-server-data"));
    let requests_per_minute = args
        .windows(2)
        .find(|window| window[0] == "--requests-per-minute")
        .and_then(|window| window[1].parse().ok())
        .unwrap_or(600);
    let db: SharedDb = Arc::new(Mutex::new(
        Db::open(&data_dir).expect("server data directory should be writable"),
    ));
    let app = app(db, requests_per_minute);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    println!(
        "auru-pm reference server listening on http://{addr}; data: {}; limit: {requests_per_minute} requests/minute/client",
        data_dir.display()
    );
    let listener = tokio::net::TcpListener::bind(addr)
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
    use super::*;
    use auru_pm_protocol::RetentionRule;

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
                "song".into(),
                StoredProject {
                    head: Some("third".into()),
                    ..StoredProject::default()
                },
            );
        }

        let response = post_retention(
            State(db.clone()),
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
                .get("song")
                .and_then(|project| project.history_floor.as_deref())
                .map(str::to_owned),
            Some("second".to_owned())
        );
    }
}

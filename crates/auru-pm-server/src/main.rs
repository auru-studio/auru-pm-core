//! In-memory `auru-pm-v1` server for development and protocol tests.
//!
//! Usage:
//!   cargo run -p auru-pm-server -- --port 4242
//!
//! The server advertises `auth_methods: ["none"]` and loses its state when
//! stopped. It must not be exposed to an untrusted network.

use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use auru_pm_protocol::{ProjectProfile, ProjectsResponse, ProviderProject, WIRE_VERSION};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use tower_http::decompression::RequestDecompressionLayer;

// ── Shared state ─────────────────────────────────────────────────────────────

#[derive(Default)]
struct Db {
    blobs: HashMap<String, Vec<u8>>,
    commits: HashMap<String, Value>,
    projects: HashMap<String, StoredProject>,
}

#[derive(Default)]
struct StoredProject {
    head: Option<String>,
    profile: Option<ProjectProfile<String>>,
    updated_at: i64,
}

type SharedDb = Arc<Mutex<Db>>;

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

fn conflict(current: Option<&str>) -> Response {
    let body = json!({"code": "head_conflict", "current": current});
    (StatusCode::CONFLICT, Json(body)).into_response()
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
            "compressed_uploads": true
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
) -> StatusCode {
    db.lock()
        .unwrap()
        .projects
        .entry(handle)
        .or_default()
        .profile = Some(profile);
    StatusCode::NO_CONTENT
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
    let project = db.projects.entry(handle).or_default();
    project.head = Some(body.to);
    project.updated_at = updated_at;
    (StatusCode::OK, Json(json!({"result": "advanced"}))).into_response()
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
    db.lock().unwrap().commits.insert(id.clone(), value);
    (StatusCode::OK, Json(json!({"id": id}))).into_response()
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

#[derive(Deserialize)]
struct HasBlobsBody {
    hashes: Vec<String>,
}

async fn post_blobs_has(
    State(db): State<SharedDb>,
    Json(body): Json<HasBlobsBody>,
) -> impl IntoResponse {
    let db = db.lock().unwrap();
    let present: Vec<bool> = body
        .hashes
        .iter()
        .map(|h| db.blobs.contains_key(h))
        .collect();
    Json(json!({ "present": present }))
}

async fn put_blob(State(db): State<SharedDb>, Path(hash): Path<String>, body: Bytes) -> Response {
    db.lock().unwrap().blobs.insert(hash, body.to_vec());
    StatusCode::OK.into_response()
}

async fn get_blob(State(db): State<SharedDb>, Path(hash): Path<String>) -> Response {
    match db.lock().unwrap().blobs.get(&hash).cloned() {
        Some(bytes) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
            bytes,
        )
            .into_response(),
        None => not_found(&format!("blob {hash}")),
    }
}

// ── Main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    let port: u16 = args
        .windows(2)
        .find(|w| w[0] == "--port")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(4242);

    let db: SharedDb = Arc::new(Mutex::new(Db::default()));

    let app = Router::new()
        .route("/v1/health", get(get_health))
        .route("/v1/projects", get(get_projects))
        .route("/v1/projects/:handle", put(put_project_profile))
        .route("/v1/projects/:handle/head", get(get_head).post(post_head))
        .route("/v1/projects/:handle/commits", post(post_commit))
        .route("/v1/projects/:handle/commits/:id", get(get_commit))
        .route("/v1/projects/:handle/history", get(get_history))
        .route("/v1/blobs/has", post(post_blobs_has))
        .route("/v1/blobs/:hash", put(put_blob).get(get_blob))
        // Unwrap `Content-Encoding: gzip` request bodies before they reach a
        // handler, so `put_blob` always stores plaintext regardless of how the
        // client chose to send it. Advertised as `compressed_uploads` in
        // `/v1/health`; clients only compress when they have seen that.
        .layer(RequestDecompressionLayer::new().gzip(true))
        .with_state(db);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    println!("auru-pm development server listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("server address should be available");
    axum::serve(listener, app)
        .await
        .expect("HTTP server should run until shutdown");
}

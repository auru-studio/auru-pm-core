//! M2 happy-path: run the same commit/history flow as the M1 filesystem test
//! but against an in-process axum stub server over real HTTP.
//!
//! The stub is embedded here (no subprocess) so the test is self-contained.
//! Tokio spawns the server as a background task on a random free port.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use auru_pm::{
    AuthorIdentity, Commit, CommitId, ContentHash, HeadAdvance, HistoryRange, HttpProvider,
    ProjectProvider, RemoteState, SampleManifest, Sidecar, TreeRef, compute_commit_id,
    sidecar_path_for,
};
use tempfile::TempDir;

// ── Embedded stub ─────────────────────────────────────────────────────────────

#[derive(Default, Clone)]
struct Db {
    blobs: HashMap<String, Vec<u8>>,
    commits: HashMap<String, Value>,
    head: Option<String>,
}

type Shared = Arc<Mutex<Db>>;

fn err_resp(status: StatusCode, code: &str, msg: &str) -> Response {
    (status, Json(json!({"code": code, "message": msg}))).into_response()
}

async fn health() -> impl IntoResponse {
    Json(json!({
        "protocol": "auru-pm-v1",
        "name": "test stub",
        "capabilities": {
            "members": false, "permissions": false,
            "branches": false, "server_side_merge": false,
            "auth_methods": ["none"]
        }
    }))
}

async fn get_head(State(db): State<Shared>) -> impl IntoResponse {
    Json(json!({ "commit_id": db.lock().unwrap().head }))
}

#[derive(Deserialize)]
struct AdvBody {
    from: Option<String>,
    to: String,
}

async fn post_head(State(db): State<Shared>, Json(b): Json<AdvBody>) -> Response {
    let mut db = db.lock().unwrap();
    if db.head.as_deref() != b.from.as_deref() {
        let cur = db.head.clone();
        drop(db);
        return (
            StatusCode::CONFLICT,
            Json(json!({"code":"head_conflict","current":cur})),
        )
            .into_response();
    }
    db.head = Some(b.to);
    (StatusCode::OK, Json(json!({"result":"advanced"}))).into_response()
}

async fn post_commit(State(db): State<Shared>, Json(commit): Json<Value>) -> Response {
    let id = match commit.get("id").and_then(Value::as_str) {
        Some(id) => id.to_owned(),
        None => return err_resp(StatusCode::BAD_REQUEST, "bad_request", "missing id"),
    };
    let mut value = commit;
    if let Value::Object(ref mut map) = value {
        map.remove("id");
    }
    db.lock().unwrap().commits.insert(id.clone(), value);
    (StatusCode::OK, Json(json!({"id": id}))).into_response()
}

async fn get_commit(State(db): State<Shared>, Path((_h, id)): Path<(String, String)>) -> Response {
    let db = db.lock().unwrap();
    match db.commits.get(&id).cloned() {
        Some(mut v) => {
            if let Value::Object(ref mut m) = v {
                m.insert("id".into(), Value::String(id));
            }
            Json(v).into_response()
        }
        None => err_resp(StatusCode::NOT_FOUND, "not_found", &format!("commit {id}")),
    }
}

#[derive(Deserialize, Default)]
struct HQ {
    limit: Option<u32>,
    before: Option<String>,
}

async fn list_history(State(db): State<Shared>, Query(q): Query<HQ>) -> impl IntoResponse {
    let db = db.lock().unwrap();
    let limit = q.limit.unwrap_or(100) as usize;
    let mut out: Vec<Value> = Vec::new();
    let mut started = q.before.is_none();
    let mut cursor = db.head.clone();
    while let Some(id) = cursor {
        let raw = match db.commits.get(&id) {
            Some(v) => v.clone(),
            None => break,
        };
        let mut full = raw.clone();
        if let Value::Object(ref mut m) = full {
            m.insert("id".into(), Value::String(id.clone()));
        }
        if started {
            out.push(json!({
                "id": full["id"], "parents": full["parents"], "author": full["author"],
                "timestamp": full["timestamp"], "message": full["message"],
                "description": full.get("description").cloned().unwrap_or_default(),
            }));
            if out.len() >= limit {
                break;
            }
        } else if Some(&id) == q.before.as_ref() {
            started = true;
        }
        cursor = raw
            .get("parents")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    Json(json!({ "commits": out }))
}

#[derive(Deserialize)]
struct HasBody {
    hashes: Vec<String>,
}

async fn blobs_has(State(db): State<Shared>, Json(b): Json<HasBody>) -> impl IntoResponse {
    let db = db.lock().unwrap();
    let present: Vec<bool> = b.hashes.iter().map(|h| db.blobs.contains_key(h)).collect();
    Json(json!({ "present": present }))
}

async fn put_blob(State(db): State<Shared>, Path(hash): Path<String>, body: Bytes) -> StatusCode {
    db.lock().unwrap().blobs.insert(hash, body.to_vec());
    StatusCode::OK
}

async fn get_blob(State(db): State<Shared>, Path(hash): Path<String>) -> Response {
    match db.lock().unwrap().blobs.get(&hash).cloned() {
        Some(b) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
            b,
        )
            .into_response(),
        None => err_resp(StatusCode::NOT_FOUND, "not_found", &format!("blob {hash}")),
    }
}

fn make_app(db: Shared) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/projects/:h/head", get(get_head).post(post_head))
        .route("/v1/projects/:h/commits", post(post_commit))
        .route("/v1/projects/:h/commits/:id", get(get_commit))
        .route("/v1/projects/:h/history", get(list_history))
        .route("/v1/blobs/has", post(blobs_has))
        .route("/v1/blobs/:hash", put(put_blob).get(get_blob))
        .with_state(db)
}

async fn start_stub() -> String {
    let db: Shared = Arc::new(Mutex::new(Db::default()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, make_app(db))
            .await
            .expect("test HTTP server should remain available");
    });
    format!("http://127.0.0.1:{port}")
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m2_probe_returns_capabilities() {
    let base = start_stub().await;
    let (name, caps) = HttpProvider::probe(&base).await.unwrap();
    assert!(!name.is_empty());
    assert!(!caps.members);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m2_commit_roundtrip_over_http() {
    let base = start_stub().await;
    let handle = "test/song";
    let provider = HttpProvider::open(&base, handle, None).await.unwrap();

    // Fresh repo has no HEAD.
    assert_eq!(provider.get_head().await.unwrap(), None);

    // Put snapshot + sample manifest blobs.
    let snapshot_bytes = br#"{"version":8,"bpm":120,"channels":[]}"#;
    let snapshot_hash = ContentHash::of(snapshot_bytes);
    let samples = SampleManifest::new();
    let samples_bytes = samples.canonical_encoding().unwrap();
    let samples_hash = ContentHash::of(&samples_bytes);

    provider
        .put_blob(&snapshot_hash, snapshot_bytes)
        .await
        .unwrap();
    provider
        .put_blob(&samples_hash, &samples_bytes)
        .await
        .unwrap();

    // has_blobs round-trip.
    let present = provider
        .has_blobs(&[snapshot_hash, samples_hash])
        .await
        .unwrap();
    assert_eq!(present, vec![true, true]);

    // Build and store a commit.
    let author = AuthorIdentity {
        display_name: "Test".into(),
        provider_user_id: "u1".into(),
        provider_id: provider.provider_id().to_owned(),
        email: None,
    };
    let mut commit = Commit {
        id: CommitId(ContentHash::ZERO),
        parents: vec![],
        tree: TreeRef {
            snapshot: snapshot_hash,
            samples: samples_hash,
        },
        author,
        timestamp: 1_700_000_000,
        message: "first take over HTTP".into(),
        description: "chorus rough draft".into(),
        auru_version: "0.1.0".into(),
        format_version: 8,
        metadata: None,
    };
    commit.id = compute_commit_id(&commit).unwrap();

    let stored_id = provider.put_commit(&commit).await.unwrap();
    assert_eq!(stored_id, commit.id);

    // Advance HEAD.
    let adv = provider.advance_head(None, commit.id).await.unwrap();
    assert_eq!(adv, HeadAdvance::Advanced);
    assert_eq!(provider.get_head().await.unwrap(), Some(commit.id));

    // list_history returns the commit.
    let history = provider
        .list_history(HistoryRange::default())
        .await
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, commit.id);
    assert_eq!(history[0].message, "first take over HTTP");
    assert_eq!(history[0].description, "chorus rough draft");

    // get_commit restores full commit.
    let fetched = provider.get_commit(&commit.id).await.unwrap();
    assert_eq!(fetched.id, commit.id);
    assert_eq!(fetched.tree.snapshot, snapshot_hash);

    // get_blob retrieves snapshot bytes.
    let blob = provider.get_blob(&snapshot_hash).await.unwrap();
    assert_eq!(blob, snapshot_bytes);

    // Sidecar round-trip alongside the test project.
    let project_dir = TempDir::new().unwrap();
    let project_path = project_dir.path().join("song.auru");
    let sidecar_path = sidecar_path_for(&project_path);
    let provider_id = provider.provider_id().to_owned();

    Sidecar::modify(&sidecar_path, |s| {
        s.primary = Some(provider_id.clone());
        s.local_head = Some(commit.id);
        s.remotes.insert(
            provider_id.clone(),
            RemoteState {
                remote_head: Some(commit.id),
                last_pulled: Some(commit.timestamp),
            },
        );
    })
    .unwrap();

    let loaded = Sidecar::load(&sidecar_path).unwrap();
    assert_eq!(loaded.local_head, Some(commit.id));
    assert_eq!(loaded.primary.as_deref(), Some(provider_id.as_str()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m2_advance_head_conflict() {
    let base = start_stub().await;
    let provider = HttpProvider::open(&base, "test/conflict", None)
        .await
        .unwrap();

    let make_commit = |msg: &str, parent: Option<CommitId>| -> Commit {
        let mut c = Commit {
            id: CommitId(ContentHash::ZERO),
            parents: parent.into_iter().collect(),
            tree: TreeRef {
                snapshot: ContentHash::of(msg.as_bytes()),
                samples: ContentHash::of(b"x"),
            },
            author: AuthorIdentity {
                display_name: "T".into(),
                provider_user_id: "u".into(),
                provider_id: "p".into(),
                email: None,
            },
            timestamp: 0,
            message: msg.into(),
            description: String::new(),
            auru_version: "0".into(),
            format_version: 8,
            metadata: None,
        };
        c.id = compute_commit_id(&c).unwrap();
        c
    };

    let c1 = make_commit("first", None);
    let c2 = make_commit("second", Some(c1.id));

    provider.put_commit(&c1).await.unwrap();
    provider.advance_head(None, c1.id).await.unwrap();

    // Stale `from` → Conflict with current HEAD.
    let bad = CommitId(ContentHash::of(b"nope"));
    provider.put_commit(&c2).await.unwrap();
    let adv = provider.advance_head(Some(bad), c2.id).await.unwrap();
    assert_eq!(
        adv,
        HeadAdvance::Conflict {
            current: Some(c1.id)
        }
    );

    // Correct from → Advanced.
    let adv = provider.advance_head(Some(c1.id), c2.id).await.unwrap();
    assert_eq!(adv, HeadAdvance::Advanced);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m2_two_commit_history() {
    let base = start_stub().await;
    let provider = HttpProvider::open(&base, "test/multi", None).await.unwrap();

    let mk = |msg: &str, parent: Option<CommitId>| -> Commit {
        let mut c = Commit {
            id: CommitId(ContentHash::ZERO),
            parents: parent.into_iter().collect(),
            tree: TreeRef {
                snapshot: ContentHash::of(msg.as_bytes()),
                samples: ContentHash::of(b"s"),
            },
            author: AuthorIdentity {
                display_name: "A".into(),
                provider_user_id: "u".into(),
                provider_id: "p".into(),
                email: None,
            },
            timestamp: 0,
            message: msg.into(),
            description: String::new(),
            auru_version: "0".into(),
            format_version: 8,
            metadata: None,
        };
        c.id = compute_commit_id(&c).unwrap();
        c
    };

    let c1 = mk("take 1", None);
    let c2 = mk("take 2", Some(c1.id));
    provider.put_commit(&c1).await.unwrap();
    provider.put_commit(&c2).await.unwrap();
    provider.advance_head(None, c1.id).await.unwrap();
    provider.advance_head(Some(c1.id), c2.id).await.unwrap();

    let history = provider
        .list_history(HistoryRange::default())
        .await
        .unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].message, "take 2");
    assert_eq!(history[1].message, "take 1");

    let limited = provider
        .list_history(HistoryRange {
            limit: Some(1),
            before: None,
        })
        .await
        .unwrap();
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].message, "take 2");
}

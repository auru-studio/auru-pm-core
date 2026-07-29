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
    AuthorIdentity, Commit, CommitId, ContentHash, HeadAdvance, HistoryRange, HttpAccount,
    HttpProvider, ProjectFormat, ProjectProfile, ProjectProvider, RemoteState, RetentionRule,
    SampleManifest, Sidecar, TreeRef, compute_commit_id, sidecar_path_for,
};
use tempfile::TempDir;

// ── Embedded stub ─────────────────────────────────────────────────────────────

#[derive(Default, Clone)]
struct Db {
    blobs: HashMap<String, Vec<u8>>,
    commits: HashMap<String, Value>,
    projects: HashMap<String, StoredProject>,
}

#[derive(Default, Clone)]
struct StoredProject {
    head: Option<String>,
    history_floor: Option<String>,
    profile: Option<ProjectProfile>,
    updated_at: i64,
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
            "project_listing": true,
            "members": false, "permissions": false,
            "branches": false, "server_side_merge": false,
            "history_retention": true,
            "auth_methods": ["none"]
        }
    }))
}

async fn get_projects(State(db): State<Shared>) -> impl IntoResponse {
    let db = db.lock().unwrap();
    let projects: Vec<Value> = db
        .projects
        .iter()
        .filter_map(|(handle, project)| {
            Some(json!({
                "handle": handle,
                "head": project.head.as_ref()?,
                "profile": project.profile.as_ref(),
                "updated_at": project.updated_at,
            }))
        })
        .collect();
    Json(json!({ "projects": projects }))
}

async fn put_profile(
    State(db): State<Shared>,
    Path(handle): Path<String>,
    Json(profile): Json<ProjectProfile>,
) -> StatusCode {
    db.lock()
        .unwrap()
        .projects
        .entry(handle)
        .or_default()
        .profile = Some(profile);
    StatusCode::NO_CONTENT
}

async fn get_head(State(db): State<Shared>, Path(handle): Path<String>) -> impl IntoResponse {
    let head = db
        .lock()
        .unwrap()
        .projects
        .get(&handle)
        .and_then(|project| project.head.clone());
    Json(json!({ "commit_id": head }))
}

#[derive(Deserialize)]
struct AdvBody {
    from: Option<String>,
    to: String,
}

async fn post_head(
    State(db): State<Shared>,
    Path(handle): Path<String>,
    Json(b): Json<AdvBody>,
) -> Response {
    let mut db = db.lock().unwrap();
    let current = db
        .projects
        .get(&handle)
        .and_then(|project| project.head.clone());
    if current.as_deref() != b.from.as_deref() {
        let cur = current;
        drop(db);
        return (
            StatusCode::CONFLICT,
            Json(json!({"code":"head_conflict","current":cur})),
        )
            .into_response();
    }
    let updated_at = db
        .commits
        .get(&b.to)
        .and_then(|commit| commit.get("timestamp"))
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let project = db.projects.entry(handle).or_default();
    project.head = Some(b.to);
    project.updated_at = updated_at;
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

async fn list_history(
    State(db): State<Shared>,
    Path(handle): Path<String>,
    Query(q): Query<HQ>,
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
        if Some(&id) == floor.as_ref() {
            break;
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

async fn retain_history(
    State(db): State<Shared>,
    Path(handle): Path<String>,
    Json(request): Json<RetentionBody>,
) -> Response {
    let mut db = db.lock().unwrap();
    let Some(project) = db.projects.get(&handle) else {
        return err_resp(StatusCode::NOT_FOUND, "not_found", "project");
    };
    let mut cursor = project.head.clone();
    let floor = project.history_floor.clone();
    let mut history = Vec::new();
    while let Some(id) = cursor {
        let Some(commit) = db.commits.get(&id) else {
            break;
        };
        history.push((
            id.clone(),
            commit
                .get("timestamp")
                .and_then(Value::as_i64)
                .unwrap_or_default(),
        ));
        if Some(&id) == floor.as_ref() {
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
        return Json(json!({
            "versions_removed": 0,
            "objects_removed": 0,
            "bytes_freed": 0
        }))
        .into_response();
    };
    db.projects
        .get_mut(&handle)
        .expect("project exists")
        .history_floor = Some(new_floor);
    Json(json!({
        "versions_removed": history.len().saturating_sub(retained_len),
        "objects_removed": 0,
        "bytes_freed": 0
    }))
    .into_response()
}

#[derive(Deserialize)]
struct RetentionBody {
    rule: RetentionRule,
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
        .route("/v1/projects", get(get_projects))
        .route("/v1/projects/:h", put(put_profile))
        .route("/v1/projects/:h/head", get(get_head).post(post_head))
        .route("/v1/projects/:h/commits", post(post_commit))
        .route("/v1/projects/:h/commits/:id", get(get_commit))
        .route("/v1/projects/:h/history", get(list_history))
        .route("/v1/projects/:h/retention", post(retain_history))
        .route("/v1/blobs/has", post(blobs_has))
        .route("/v1/blobs/:hash", put(put_blob).get(get_blob))
        .with_state(db)
}

async fn start_stub_with_db() -> (String, Shared) {
    let db: Shared = Arc::new(Mutex::new(Db::default()));
    let server_db = db.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, make_app(server_db))
            .await
            .expect("test HTTP server should remain available");
    });
    (format!("http://127.0.0.1:{port}"), db)
}

async fn start_stub() -> String {
    start_stub_with_db().await.0
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m2_probe_returns_capabilities() {
    let base = start_stub().await;
    let (name, caps) = HttpProvider::probe(&base).await.unwrap();
    assert!(!name.is_empty());
    assert!(caps.project_listing);
    assert!(!caps.members);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m2_corrupt_blob_download_is_rejected() {
    let (base, db) = start_stub_with_db().await;
    let requested = ContentHash::of(b"expected bytes");
    db.lock()
        .unwrap()
        .blobs
        .insert(requested.to_string(), b"wrong bytes".to_vec());
    let provider = HttpProvider::open(&base, "test/corruption", None)
        .await
        .unwrap();

    let error = provider
        .get_blob(&requested)
        .await
        .expect_err("corrupt response must not reach a restore");

    assert!(error.to_string().contains("blob hash mismatch"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m2_commit_roundtrip_over_http() {
    let base = start_stub().await;
    let handle = "test/song";
    let account = HttpAccount::connect(&base, None).await.unwrap();
    let provider = account.open_project(handle);
    let profile = ProjectProfile {
        display_name: "Night Drive".into(),
        format: ProjectFormat::Auru,
    };
    provider.put_project_profile(&profile).await.unwrap();

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

    let projects = account.list_projects().await.unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].handle, handle);
    assert_eq!(projects[0].head, commit.id);
    assert_eq!(projects[0].profile.as_ref(), Some(&profile));
    assert_eq!(projects[0].updated_at, commit.timestamp);

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

    let report = provider
        .prune_history(
            RetentionRule::Latest { count: 1 },
            &auru_pm::RetentionRoots::default(),
        )
        .await
        .unwrap();
    assert_eq!(report.versions_removed, 1);
    let retained = provider
        .list_history(HistoryRange::default())
        .await
        .unwrap();
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].message, "take 2");
}

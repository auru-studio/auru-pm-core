//! Compressed blob uploads, and the negotiation that keeps them safe.
//!
//! A project snapshot is canonical JSON and compresses about sevenfold, so
//! compressing uploads is most of the transfer for a project of any size. The
//! hazard is that request-body compression cannot be negotiated by the request
//! itself: a server that ignores `Content-Encoding` would store the compressed
//! bytes under the plaintext hash and corrupt the blob, and nothing would
//! notice until someone tried to open that version.
//!
//! So the server declares support in `/v1/health` and the client compresses
//! only when it has seen that declaration. These tests pin both directions.

use std::sync::{Arc, Mutex};

use auru_pm::{ContentHash, HttpProvider, ProjectProvider};
use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::{get, put};
use axum::{Json, Router, body::Bytes};
use serde_json::json;
use tower_http::decompression::RequestDecompressionLayer;

/// What the stub observed, so tests can assert on the wire and not just the
/// end result.
#[derive(Default)]
struct Observed {
    blobs: std::collections::HashMap<String, Vec<u8>>,
    /// `Content-Encoding` as it arrived, before any decompression layer.
    encodings: Vec<Option<String>>,
}

type Shared = Arc<Mutex<Observed>>;

/// A snapshot-shaped payload — repetitive JSON, like a real canonical tree.
fn compressible_payload() -> Vec<u8> {
    let mut json = String::from(r#"{"root":{"tag":"Ableton","children":["#);
    for index in 0..4_000 {
        json.push_str(&format!(
            r#"{{"tag":"MidiTrack","id":"{index}","attributes":{{"Value":"0"}}}},"#
        ));
    }
    json.push_str("null]}}");
    json.into_bytes()
}

async fn health(State(compressed_uploads): State<bool>) -> Json<serde_json::Value> {
    Json(json!({
        "protocol": auru_pm::WIRE_VERSION,
        "name": "compression stub",
        "capabilities": {
            "members": false, "permissions": false,
            "branches": false, "server_side_merge": false,
            "auth_methods": ["none"],
            "compressed_uploads": compressed_uploads
        }
    }))
}

async fn put_blob(State(db): State<Shared>, Path(hash): Path<String>, body: Bytes) -> StatusCode {
    db.lock().expect("lock").blobs.insert(hash, body.to_vec());
    StatusCode::OK
}

/// Record the request's `Content-Encoding` before anything strips it.
async fn record_encoding(State(db): State<Shared>, request: Request, next: Next) -> Response {
    let encoding = request
        .headers()
        .get(header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    db.lock().expect("lock").encodings.push(encoding);
    next.run(request).await
}

/// Start a stub that advertises `compressed_uploads` as given, and decodes
/// gzip request bodies when it does.
async fn start_stub(compressed_uploads: bool) -> (String, Shared) {
    let db: Shared = Arc::new(Mutex::new(Observed::default()));

    let blobs = Router::new()
        .route("/v1/blobs/:hash", put(put_blob))
        .with_state(db.clone());
    // Layers wrap outward, so `record_encoding` — added last — runs before
    // decompression and sees the header as it arrived.
    let blobs = if compressed_uploads {
        blobs.layer(RequestDecompressionLayer::new().gzip(true))
    } else {
        blobs
    };
    let blobs = blobs.layer(axum::middleware::from_fn_with_state(
        db.clone(),
        record_encoding,
    ));

    let app = Router::new()
        .route("/v1/health", get(health))
        .with_state(compressed_uploads)
        .merge(blobs);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), db)
}

#[tokio::test]
async fn a_server_that_decodes_gzip_should_receive_a_compressed_body() {
    let (base, db) = start_stub(true).await;
    let provider = HttpProvider::open(&base, "proj", None)
        .await
        .expect("open provider");

    let payload = compressible_payload();
    let hash = ContentHash::of(&payload);
    provider.put_blob(&hash, &payload).await.expect("put blob");

    let observed = db.lock().expect("lock");
    assert_eq!(
        observed.encodings,
        vec![Some("gzip".to_owned())],
        "the body should have gone over the wire compressed"
    );
    assert_eq!(
        observed.blobs.get(&hash.to_string()),
        Some(&payload),
        "and the server should store the plaintext it decoded"
    );
}

#[tokio::test]
async fn a_server_that_does_not_advertise_support_should_receive_plaintext() {
    // The safety property. A server written before this capability existed
    // omits the field, the client reads `false`, and the body is sent as-is.
    // Compressing anyway would have the server store gzip under the plaintext
    // hash — corruption that surfaces only when the version is next opened.
    let (base, db) = start_stub(false).await;
    let provider = HttpProvider::open(&base, "proj", None)
        .await
        .expect("open provider");

    let payload = compressible_payload();
    let hash = ContentHash::of(&payload);
    provider.put_blob(&hash, &payload).await.expect("put blob");

    let observed = db.lock().expect("lock");
    assert_eq!(
        observed.encodings,
        vec![None],
        "no Content-Encoding may be sent to a server that did not ask for it"
    );
    assert_eq!(
        observed.blobs.get(&hash.to_string()),
        Some(&payload),
        "the stored bytes must hash to the name in the URL"
    );
}

#[tokio::test]
async fn incompressible_blobs_should_be_sent_uncompressed() {
    // Audio does not shrink. Sending a larger body with a Content-Encoding
    // header would cost bandwidth to achieve nothing.
    let (base, db) = start_stub(true).await;
    let provider = HttpProvider::open(&base, "proj", None)
        .await
        .expect("open provider");

    let payload: Vec<u8> = (0..4_096_u32)
        .flat_map(|index| index.wrapping_mul(2_654_435_761).to_le_bytes())
        .collect();
    let hash = ContentHash::of(&payload);
    provider.put_blob(&hash, &payload).await.expect("put blob");

    let observed = db.lock().expect("lock");
    assert_eq!(observed.encodings, vec![None]);
    assert_eq!(observed.blobs.get(&hash.to_string()), Some(&payload));
}

#[tokio::test]
async fn capabilities_should_default_to_no_compressed_uploads() {
    // Explicitly pinned because the default is what protects every server
    // that predates the field.
    let capabilities: auru_pm::Capabilities = serde_json::from_value(json!({
        "members": false, "permissions": false,
        "branches": false, "server_side_merge": false,
        "auth_methods": ["none"]
    }))
    .expect("decode capabilities without the field");
    assert!(!capabilities.compressed_uploads);
}

//! Browser binding.
//!
//! Thin wrappers over [`crate::surface`]; every one is a single call. There is
//! deliberately no filesystem here and no HTTP: a browser has no disk, and
//! transport belongs to the TypeScript side.

use wasm_bindgen::prelude::*;

use crate::surface;

fn js(error: String) -> JsError {
    JsError::new(&error)
}

#[wasm_bindgen(js_name = protocolVersion)]
pub fn protocol_version() -> String {
    surface::protocol_version().to_owned()
}

#[wasm_bindgen(js_name = commitId)]
pub fn commit_id(commit_json: &str) -> Result<String, JsError> {
    surface::commit_id(commit_json).map_err(js)
}

#[wasm_bindgen(js_name = commitCanonicalEncoding)]
pub fn commit_canonical_encoding(commit_json: &str) -> Result<Vec<u8>, JsError> {
    surface::commit_canonical_encoding(commit_json).map_err(js)
}

#[wasm_bindgen(js_name = contentHash)]
pub fn content_hash(bytes: &[u8]) -> String {
    surface::content_hash(bytes)
}

#[wasm_bindgen(js_name = verifyBlob)]
pub fn verify_blob(bytes: &[u8], expected: &str) -> Result<bool, JsError> {
    surface::verify_blob(bytes, expected).map_err(js)
}

#[wasm_bindgen(js_name = projectInfoFromSnapshot)]
pub fn project_info_from_snapshot(snapshot: &[u8]) -> Result<Option<String>, JsError> {
    surface::project_info_from_snapshot(snapshot).map_err(js)
}

#[wasm_bindgen(js_name = projectInfoFromBlob)]
pub fn project_info_from_blob(blob: &[u8]) -> Result<String, JsError> {
    surface::project_info_from_blob(blob).map_err(js)
}

#[wasm_bindgen(js_name = detectFormat)]
pub fn detect_format(file_name: &str, source: &[u8]) -> Result<String, JsError> {
    surface::detect_format(file_name, source).map_err(js)
}

#[wasm_bindgen(js_name = snapshotFromSource)]
pub fn snapshot_from_source(format: &str, source: &[u8]) -> Result<Vec<u8>, JsError> {
    surface::snapshot_from_source(format, source).map_err(js)
}

#[wasm_bindgen(js_name = restoreFromSnapshot)]
pub fn restore_from_snapshot(canonical: &[u8]) -> Result<Vec<u8>, JsError> {
    surface::restore_from_snapshot(canonical).map_err(js)
}

#[wasm_bindgen(js_name = diffSnapshots)]
pub fn diff_snapshots(before: &[u8], after: &[u8]) -> Result<String, JsError> {
    surface::diff_snapshots(before, after).map_err(js)
}

#[wasm_bindgen(js_name = summarizeSnapshots)]
pub fn summarize_snapshots(before: &[u8], after: &[u8]) -> Result<String, JsError> {
    surface::summarize_snapshots(before, after).map_err(js)
}

#[wasm_bindgen(js_name = mergeSnapshots)]
pub fn merge_snapshots(ancestor: &[u8], local: &[u8], remote: &[u8]) -> Result<String, JsError> {
    surface::merge_snapshots(ancestor, local, remote).map_err(js)
}

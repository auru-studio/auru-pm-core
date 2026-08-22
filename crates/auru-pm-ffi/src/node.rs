//! Node and Electron binding.
//!
//! The same surface as the browser build, wrapped for N-API. Node could reach
//! the filesystem, but these functions still take and return bytes: the caller
//! knows where a project lives, and keeping both bindings byte-for-byte
//! identical means a dashboard and an Electron client share one code path.
//!
//! Transport is not here either. TypeScript owns the network in both runtimes.

use napi::bindgen_prelude::{Result, Uint8Array};
use napi_derive::napi;

use crate::surface;

fn err(error: String) -> napi::Error {
    napi::Error::from_reason(error)
}

#[napi]
pub fn protocol_version() -> String {
    surface::protocol_version().to_owned()
}

#[napi]
pub fn commit_id(commit_json: String) -> Result<String> {
    surface::commit_id(&commit_json).map_err(err)
}

#[napi]
pub fn commit_canonical_encoding(commit_json: String) -> Result<Uint8Array> {
    surface::commit_canonical_encoding(&commit_json)
        .map(Uint8Array::new)
        .map_err(err)
}

#[napi]
pub fn content_hash(bytes: Uint8Array) -> String {
    surface::content_hash(&bytes)
}

#[napi]
pub fn verify_blob(bytes: Uint8Array, expected: String) -> Result<bool> {
    surface::verify_blob(&bytes, &expected).map_err(err)
}

#[napi]
pub fn project_info_from_snapshot(snapshot: Uint8Array) -> Result<Option<String>> {
    surface::project_info_from_snapshot(&snapshot).map_err(err)
}

#[napi]
pub fn project_info_from_blob(blob: Uint8Array) -> Result<String> {
    surface::project_info_from_blob(&blob).map_err(err)
}

#[napi]
pub fn detect_format(file_name: String, source: Uint8Array) -> Result<String> {
    surface::detect_format(&file_name, &source).map_err(err)
}

#[napi]
pub fn snapshot_from_source(format: String, source: Uint8Array) -> Result<Uint8Array> {
    surface::snapshot_from_source(&format, &source)
        .map(Uint8Array::new)
        .map_err(err)
}

#[napi]
pub fn restore_from_snapshot(canonical: Uint8Array) -> Result<Uint8Array> {
    surface::restore_from_snapshot(&canonical)
        .map(Uint8Array::new)
        .map_err(err)
}

#[napi]
pub fn diff_snapshots(before: Uint8Array, after: Uint8Array) -> Result<String> {
    surface::diff_snapshots(&before, &after).map_err(err)
}

#[napi]
pub fn summarize_snapshots(before: Uint8Array, after: Uint8Array) -> Result<String> {
    surface::summarize_snapshots(&before, &after).map_err(err)
}

#[napi]
pub fn merge_snapshots(
    ancestor: Uint8Array,
    local: Uint8Array,
    remote: Uint8Array,
) -> Result<String> {
    surface::merge_snapshots(&ancestor, &local, &remote).map_err(err)
}

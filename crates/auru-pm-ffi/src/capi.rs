//! C ABI binding.
//!
//! For Swift, and for anything else that speaks C. Unlike the wasm and N-API
//! bindings, the caller here owns memory management, so the contract has to be
//! stated rather than inferred:
//!
//! - Every call returns an [`AuruBuffer`]. When `error` is zero it holds the
//!   result; when it is one it holds a UTF-8 error message.
//! - Every returned buffer must be released with [`auru_pm_buffer_free`],
//!   exactly once. Freeing it with the platform allocator instead will corrupt
//!   the heap: Rust allocated it, so Rust has to release it.
//! - Input pointers are borrowed for the duration of the call and never
//!   retained.
//!
//! All functions are safe to call from any thread; nothing here holds state.
//!
//! The workspace denies `unsafe_code`, and this module is the one place that
//! exception is made. That is what the deny is for: a C ABI cannot be written
//! without raw pointers, so the exception is narrowed to this file and stated
//! here rather than relaxed across the crate. Every `unsafe fn` below documents
//! what a caller must guarantee, and nothing in the module holds a raw pointer
//! past the end of a call.
#![allow(unsafe_code)]

use std::os::raw::c_int;

use crate::surface;

/// A block of bytes owned by this library.
///
/// `data` is never null when `len` is non-zero. An empty successful result has
/// a null `data` and a zero `len`, which is still valid to pass to
/// [`auru_pm_buffer_free`].
#[repr(C)]
pub struct AuruBuffer {
    pub data: *mut u8,
    pub len: usize,
    /// Zero on success. One when `data` holds a UTF-8 error message instead.
    pub error: u8,
}

impl AuruBuffer {
    fn from_vec(mut bytes: Vec<u8>, error: u8) -> Self {
        bytes.shrink_to_fit();
        let buffer = AuruBuffer {
            data: bytes.as_mut_ptr(),
            len: bytes.len(),
            error,
        };
        // Ownership passes to the caller, who returns it via
        // `auru_pm_buffer_free`.
        std::mem::forget(bytes);
        buffer
    }

    fn ok(bytes: Vec<u8>) -> Self {
        Self::from_vec(bytes, 0)
    }

    fn err(message: String) -> Self {
        Self::from_vec(message.into_bytes(), 1)
    }

    fn from_result(result: Result<Vec<u8>, String>) -> Self {
        match result {
            Ok(bytes) => Self::ok(bytes),
            Err(message) => Self::err(message),
        }
    }

    fn from_text(result: Result<String, String>) -> Self {
        Self::from_result(result.map(String::into_bytes))
    }
}

/// Release a buffer returned by this library.
///
/// # Safety
///
/// `buffer` must have come from one of this library's functions and must not
/// have been freed already.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn auru_pm_buffer_free(buffer: AuruBuffer) {
    if !buffer.data.is_null() {
        drop(unsafe { Vec::from_raw_parts(buffer.data, buffer.len, buffer.len) });
    }
}

/// Borrow an input slice. Empty when the pointer is null.
///
/// # Safety
///
/// `data` must be valid for `len` bytes, or null.
unsafe fn slice<'a>(data: *const u8, len: usize) -> &'a [u8] {
    if data.is_null() || len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(data, len) }
    }
}

/// Borrow an input string. Invalid UTF-8 becomes an empty string, which every
/// caller then rejects with a parse error naming the input.
///
/// # Safety
///
/// `data` must be valid for `len` bytes, or null.
unsafe fn text<'a>(data: *const u8, len: usize) -> &'a str {
    std::str::from_utf8(unsafe { slice(data, len) }).unwrap_or("")
}

/// The wire protocol version this build speaks.
#[unsafe(no_mangle)]
pub extern "C" fn auru_pm_protocol_version() -> AuruBuffer {
    AuruBuffer::ok(surface::protocol_version().as_bytes().to_vec())
}

/// Derive a commit's id from its content.
///
/// # Safety
///
/// `json` must be valid for `json_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn auru_pm_commit_id(json: *const u8, json_len: usize) -> AuruBuffer {
    AuruBuffer::from_text(surface::commit_id(unsafe { text(json, json_len) }))
}

/// The exact bytes a commit's id is the BLAKE3 of.
///
/// # Safety
///
/// `json` must be valid for `json_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn auru_pm_commit_canonical_encoding(
    json: *const u8,
    json_len: usize,
) -> AuruBuffer {
    AuruBuffer::from_result(surface::commit_canonical_encoding(unsafe {
        text(json, json_len)
    }))
}

/// BLAKE3 of `bytes`, as `blake3:<64 hex>`.
///
/// # Safety
///
/// `bytes` must be valid for `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn auru_pm_content_hash(bytes: *const u8, len: usize) -> AuruBuffer {
    AuruBuffer::ok(surface::content_hash(unsafe { slice(bytes, len) }).into_bytes())
}

/// Whether `bytes` hash to `expected`.
///
/// Returns 1 for yes, 0 for no, and -1 when `expected` is not a hash — a
/// distinction a boolean could not carry, and one a caller must not silently
/// read as "no".
///
/// # Safety
///
/// Both pointers must be valid for their stated lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn auru_pm_verify_blob(
    bytes: *const u8,
    len: usize,
    expected: *const u8,
    expected_len: usize,
) -> c_int {
    match surface::verify_blob(unsafe { slice(bytes, len) }, unsafe {
        text(expected, expected_len)
    }) {
        Ok(true) => 1,
        Ok(false) => 0,
        Err(_) => -1,
    }
}

/// Summarize a canonical snapshot.
///
/// A successful empty buffer means the snapshot is of a format this build
/// cannot summarize; the caller falls back to reading the snapshot itself.
///
/// # Safety
///
/// `snapshot` must be valid for `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn auru_pm_project_info_from_snapshot(
    snapshot: *const u8,
    len: usize,
) -> AuruBuffer {
    match surface::project_info_from_snapshot(unsafe { slice(snapshot, len) }) {
        Ok(Some(json)) => AuruBuffer::ok(json.into_bytes()),
        Ok(None) => AuruBuffer::ok(Vec::new()),
        Err(message) => AuruBuffer::err(message),
    }
}

/// Identify a project file from its name and leading bytes.
///
/// # Safety
///
/// Both pointers must be valid for their stated lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn auru_pm_detect_format(
    file_name: *const u8,
    file_name_len: usize,
    source: *const u8,
    source_len: usize,
) -> AuruBuffer {
    AuruBuffer::from_text(surface::detect_format(
        unsafe { text(file_name, file_name_len) },
        unsafe { slice(source, source_len) },
    ))
}

/// Normalize a project file into canonical snapshot bytes.
///
/// # Safety
///
/// Both pointers must be valid for their stated lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn auru_pm_snapshot_from_source(
    format: *const u8,
    format_len: usize,
    source: *const u8,
    source_len: usize,
) -> AuruBuffer {
    AuruBuffer::from_result(surface::snapshot_from_source(
        unsafe { text(format, format_len) },
        unsafe { slice(source, source_len) },
    ))
}

/// Rebuild the original project file from canonical snapshot bytes.
///
/// # Safety
///
/// `canonical` must be valid for `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn auru_pm_restore_from_snapshot(
    canonical: *const u8,
    len: usize,
) -> AuruBuffer {
    AuruBuffer::from_result(surface::restore_from_snapshot(unsafe {
        slice(canonical, len)
    }))
}

/// Per-channel structured diff between two canonical snapshots.
///
/// # Safety
///
/// Both pointers must be valid for their stated lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn auru_pm_diff_snapshots(
    before: *const u8,
    before_len: usize,
    after: *const u8,
    after_len: usize,
) -> AuruBuffer {
    AuruBuffer::from_text(surface::diff_snapshots(
        unsafe { slice(before, before_len) },
        unsafe { slice(after, after_len) },
    ))
}

/// One line per change, between two canonical snapshots.
///
/// # Safety
///
/// Both pointers must be valid for their stated lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn auru_pm_summarize_snapshots(
    before: *const u8,
    before_len: usize,
    after: *const u8,
    after_len: usize,
) -> AuruBuffer {
    AuruBuffer::from_text(surface::summarize_snapshots(
        unsafe { slice(before, before_len) },
        unsafe { slice(after, after_len) },
    ))
}

/// Three-way merge of canonical snapshots.
///
/// # Safety
///
/// All three pointers must be valid for their stated lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn auru_pm_merge_snapshots(
    ancestor: *const u8,
    ancestor_len: usize,
    local: *const u8,
    local_len: usize,
    remote: *const u8,
    remote_len: usize,
) -> AuruBuffer {
    AuruBuffer::from_text(surface::merge_snapshots(
        unsafe { slice(ancestor, ancestor_len) },
        unsafe { slice(local, local_len) },
        unsafe { slice(remote, remote_len) },
    ))
}

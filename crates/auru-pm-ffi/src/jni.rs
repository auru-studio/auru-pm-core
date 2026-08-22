//! JNI binding, for Android.
//!
//! Mirrors the C ABI but with JNI's memory model instead: the JVM owns every
//! object crossing the boundary, so nothing here needs a free function and a
//! caller cannot leak by forgetting one.
//!
//! Errors become Java exceptions rather than sentinel values, which is what a
//! Kotlin or Java caller expects and what makes a mistake impossible to ignore.
//!
//! The Java side is `studio.auru.pm.Kernel`; the symbol names below have to
//! match that package exactly.
#![allow(unsafe_code)]

use jni::JNIEnv;
use jni::objects::{JByteArray, JClass, JString};
use jni::sys::{jbyteArray, jint, jstring};

use crate::surface;

/// Throw an `AuruKernelException` carrying `message`.
///
/// Returning a null handle afterwards is the JNI convention: the JVM checks for
/// a pending exception on return and never looks at the value.
fn throw(env: &mut JNIEnv, message: &str) {
    let _ = env.throw_new("studio/auru/pm/AuruKernelException", message);
}

fn read_string(env: &mut JNIEnv, value: &JString) -> Option<String> {
    match env.get_string(value) {
        Ok(text) => Some(text.into()),
        Err(error) => {
            throw(env, &format!("cannot read a string argument: {error}"));
            None
        }
    }
}

fn read_bytes(env: &mut JNIEnv, value: &JByteArray) -> Option<Vec<u8>> {
    match env.convert_byte_array(value) {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            throw(env, &format!("cannot read a byte array argument: {error}"));
            None
        }
    }
}

fn to_jstring(env: &mut JNIEnv, value: String) -> jstring {
    match env.new_string(value) {
        Ok(text) => text.into_raw(),
        Err(error) => {
            throw(env, &format!("cannot allocate a string result: {error}"));
            std::ptr::null_mut()
        }
    }
}

fn to_jbytes(env: &mut JNIEnv, value: Vec<u8>) -> jbyteArray {
    match env.byte_array_from_slice(&value) {
        Ok(array) => array.into_raw(),
        Err(error) => {
            throw(
                env,
                &format!("cannot allocate a byte array result: {error}"),
            );
            std::ptr::null_mut()
        }
    }
}

/// Unwrap a surface result, throwing and returning null on failure.
macro_rules! or_throw {
    ($env:expr, $result:expr, $null:expr) => {
        match $result {
            Ok(value) => value,
            Err(message) => {
                throw($env, &message);
                return $null;
            }
        }
    };
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_studio_auru_pm_Kernel_protocolVersion(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    to_jstring(&mut env, surface::protocol_version().to_owned())
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_studio_auru_pm_Kernel_commitId(
    mut env: JNIEnv,
    _class: JClass,
    commit_json: JString,
) -> jstring {
    let Some(json) = read_string(&mut env, &commit_json) else {
        return std::ptr::null_mut();
    };
    let id = or_throw!(&mut env, surface::commit_id(&json), std::ptr::null_mut());
    to_jstring(&mut env, id)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_studio_auru_pm_Kernel_commitCanonicalEncoding(
    mut env: JNIEnv,
    _class: JClass,
    commit_json: JString,
) -> jbyteArray {
    let Some(json) = read_string(&mut env, &commit_json) else {
        return std::ptr::null_mut();
    };
    let bytes = or_throw!(
        &mut env,
        surface::commit_canonical_encoding(&json),
        std::ptr::null_mut()
    );
    to_jbytes(&mut env, bytes)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_studio_auru_pm_Kernel_contentHash(
    mut env: JNIEnv,
    _class: JClass,
    bytes: JByteArray,
) -> jstring {
    let Some(data) = read_bytes(&mut env, &bytes) else {
        return std::ptr::null_mut();
    };
    to_jstring(&mut env, surface::content_hash(&data))
}

/// Returns 1 for yes, 0 for no. Throws when `expected` is not a hash — a
/// distinction a boolean could not carry, and one a caller must not read as
/// "no".
#[unsafe(no_mangle)]
pub extern "system" fn Java_studio_auru_pm_Kernel_verifyBlob(
    mut env: JNIEnv,
    _class: JClass,
    bytes: JByteArray,
    expected: JString,
) -> jint {
    let Some(data) = read_bytes(&mut env, &bytes) else {
        return 0;
    };
    let Some(hash) = read_string(&mut env, &expected) else {
        return 0;
    };
    jint::from(or_throw!(&mut env, surface::verify_blob(&data, &hash), 0))
}

/// Null means the snapshot is of a format this build cannot summarize, and the
/// caller falls back to reading the snapshot itself. An exception means
/// something went wrong; the two are deliberately different outcomes.
#[unsafe(no_mangle)]
pub extern "system" fn Java_studio_auru_pm_Kernel_projectInfoFromSnapshot(
    mut env: JNIEnv,
    _class: JClass,
    snapshot: JByteArray,
) -> jstring {
    let Some(data) = read_bytes(&mut env, &snapshot) else {
        return std::ptr::null_mut();
    };
    match or_throw!(
        &mut env,
        surface::project_info_from_snapshot(&data),
        std::ptr::null_mut()
    ) {
        Some(json) => to_jstring(&mut env, json),
        None => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_studio_auru_pm_Kernel_detectFormat(
    mut env: JNIEnv,
    _class: JClass,
    file_name: JString,
    source: JByteArray,
) -> jstring {
    let Some(name) = read_string(&mut env, &file_name) else {
        return std::ptr::null_mut();
    };
    let Some(data) = read_bytes(&mut env, &source) else {
        return std::ptr::null_mut();
    };
    let format = or_throw!(
        &mut env,
        surface::detect_format(&name, &data),
        std::ptr::null_mut()
    );
    to_jstring(&mut env, format)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_studio_auru_pm_Kernel_snapshotFromSource(
    mut env: JNIEnv,
    _class: JClass,
    format: JString,
    source: JByteArray,
) -> jbyteArray {
    let Some(format) = read_string(&mut env, &format) else {
        return std::ptr::null_mut();
    };
    let Some(data) = read_bytes(&mut env, &source) else {
        return std::ptr::null_mut();
    };
    let snapshot = or_throw!(
        &mut env,
        surface::snapshot_from_source(&format, &data),
        std::ptr::null_mut()
    );
    to_jbytes(&mut env, snapshot)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_studio_auru_pm_Kernel_restoreFromSnapshot(
    mut env: JNIEnv,
    _class: JClass,
    canonical: JByteArray,
) -> jbyteArray {
    let Some(data) = read_bytes(&mut env, &canonical) else {
        return std::ptr::null_mut();
    };
    let restored = or_throw!(
        &mut env,
        surface::restore_from_snapshot(&data),
        std::ptr::null_mut()
    );
    to_jbytes(&mut env, restored)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_studio_auru_pm_Kernel_diffSnapshots(
    mut env: JNIEnv,
    _class: JClass,
    before: JByteArray,
    after: JByteArray,
) -> jstring {
    let (Some(before), Some(after)) = (read_bytes(&mut env, &before), read_bytes(&mut env, &after))
    else {
        return std::ptr::null_mut();
    };
    let diff = or_throw!(
        &mut env,
        surface::diff_snapshots(&before, &after),
        std::ptr::null_mut()
    );
    to_jstring(&mut env, diff)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_studio_auru_pm_Kernel_summarizeSnapshots(
    mut env: JNIEnv,
    _class: JClass,
    before: JByteArray,
    after: JByteArray,
) -> jstring {
    let (Some(before), Some(after)) = (read_bytes(&mut env, &before), read_bytes(&mut env, &after))
    else {
        return std::ptr::null_mut();
    };
    let summary = or_throw!(
        &mut env,
        surface::summarize_snapshots(&before, &after),
        std::ptr::null_mut()
    );
    to_jstring(&mut env, summary)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_studio_auru_pm_Kernel_mergeSnapshots(
    mut env: JNIEnv,
    _class: JClass,
    ancestor: JByteArray,
    local: JByteArray,
    remote: JByteArray,
) -> jstring {
    let (Some(ancestor), Some(local), Some(remote)) = (
        read_bytes(&mut env, &ancestor),
        read_bytes(&mut env, &local),
        read_bytes(&mut env, &remote),
    ) else {
        return std::ptr::null_mut();
    };
    let merged = or_throw!(
        &mut env,
        surface::merge_snapshots(&ancestor, &local, &remote),
        std::ptr::null_mut()
    );
    to_jstring(&mut env, merged)
}

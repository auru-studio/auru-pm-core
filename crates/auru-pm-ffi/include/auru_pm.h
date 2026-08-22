/*
 * auru-pm C ABI.
 *
 * Generated from crates/auru-pm-ffi/src/capi.rs by cbindgen. Do not edit.
 *
 * Memory: every call returns an AuruBuffer that the caller must release with
 * auru_pm_buffer_free, exactly once, and never with free(). Rust allocated it,
 * so Rust has to release it.
 *
 * Errors: AuruBuffer.error is 0 on success, 1 when the buffer holds a UTF-8
 * error message instead of a result.
 */


#ifndef AURU_PM_H
#define AURU_PM_H



#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

/**
 * A block of bytes owned by this library.
 *
 * `data` is never null when `len` is non-zero. An empty successful result has
 * a null `data` and a zero `len`, which is still valid to pass to
 * [`auru_pm_buffer_free`].
 */
typedef struct AuruBuffer {
  uint8_t *data;
  uintptr_t len;
  /**
   * Zero on success. One when `data` holds a UTF-8 error message instead.
   */
  uint8_t error;
} AuruBuffer;

#ifdef __cplusplus
extern "C" {
#endif // __cplusplus

/**
 * Release a buffer returned by this library.
 *
 * # Safety
 *
 * `buffer` must have come from one of this library's functions and must not
 * have been freed already.
 */
void auru_pm_buffer_free(struct AuruBuffer buffer);

/**
 * The wire protocol version this build speaks.
 */
struct AuruBuffer auru_pm_protocol_version(void);

/**
 * Derive a commit's id from its content.
 *
 * # Safety
 *
 * `json` must be valid for `json_len` bytes.
 */
struct AuruBuffer auru_pm_commit_id(const uint8_t *json, uintptr_t json_len);

/**
 * The exact bytes a commit's id is the BLAKE3 of.
 *
 * # Safety
 *
 * `json` must be valid for `json_len` bytes.
 */
struct AuruBuffer auru_pm_commit_canonical_encoding(const uint8_t *json, uintptr_t json_len);

/**
 * BLAKE3 of `bytes`, as `blake3:<64 hex>`.
 *
 * # Safety
 *
 * `bytes` must be valid for `len` bytes.
 */
struct AuruBuffer auru_pm_content_hash(const uint8_t *bytes, uintptr_t len);

/**
 * Whether `bytes` hash to `expected`.
 *
 * Returns 1 for yes, 0 for no, and -1 when `expected` is not a hash — a
 * distinction a boolean could not carry, and one a caller must not silently
 * read as "no".
 *
 * # Safety
 *
 * Both pointers must be valid for their stated lengths.
 */
int auru_pm_verify_blob(const uint8_t *bytes,
                        uintptr_t len,
                        const uint8_t *expected,
                        uintptr_t expected_len);

/**
 * Summarize a canonical snapshot.
 *
 * A successful empty buffer means the snapshot is of a format this build
 * cannot summarize; the caller falls back to reading the snapshot itself.
 *
 * # Safety
 *
 * `snapshot` must be valid for `len` bytes.
 */
struct AuruBuffer auru_pm_project_info_from_snapshot(const uint8_t *snapshot, uintptr_t len);

/**
 * Identify a project file from its name and leading bytes.
 *
 * # Safety
 *
 * Both pointers must be valid for their stated lengths.
 */
struct AuruBuffer auru_pm_detect_format(const uint8_t *file_name,
                                        uintptr_t file_name_len,
                                        const uint8_t *source,
                                        uintptr_t source_len);

/**
 * Normalize a project file into canonical snapshot bytes.
 *
 * # Safety
 *
 * Both pointers must be valid for their stated lengths.
 */
struct AuruBuffer auru_pm_snapshot_from_source(const uint8_t *format,
                                               uintptr_t format_len,
                                               const uint8_t *source,
                                               uintptr_t source_len);

/**
 * Rebuild the original project file from canonical snapshot bytes.
 *
 * # Safety
 *
 * `canonical` must be valid for `len` bytes.
 */
struct AuruBuffer auru_pm_restore_from_snapshot(const uint8_t *canonical, uintptr_t len);

/**
 * Per-channel structured diff between two canonical snapshots.
 *
 * # Safety
 *
 * Both pointers must be valid for their stated lengths.
 */
struct AuruBuffer auru_pm_diff_snapshots(const uint8_t *before,
                                         uintptr_t before_len,
                                         const uint8_t *after,
                                         uintptr_t after_len);

/**
 * One line per change, between two canonical snapshots.
 *
 * # Safety
 *
 * Both pointers must be valid for their stated lengths.
 */
struct AuruBuffer auru_pm_summarize_snapshots(const uint8_t *before,
                                              uintptr_t before_len,
                                              const uint8_t *after,
                                              uintptr_t after_len);

/**
 * Three-way merge of canonical snapshots.
 *
 * # Safety
 *
 * All three pointers must be valid for their stated lengths.
 */
struct AuruBuffer auru_pm_merge_snapshots(const uint8_t *ancestor,
                                          uintptr_t ancestor_len,
                                          const uint8_t *local,
                                          uintptr_t local_len,
                                          const uint8_t *remote,
                                          uintptr_t remote_len);

#ifdef __cplusplus
}  // extern "C"
#endif  // __cplusplus

#endif  /* AURU_PM_H */

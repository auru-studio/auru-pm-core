//! The portable core of Auru PM: what a project *is*, and what a commit *is*.
//!
//! Everything here is a pure function of bytes. No filesystem, no sockets, no
//! keychain, no platform APIs — so it builds for `wasm32-unknown-unknown` and
//! runs unchanged in a browser, in Node, and in the desktop app.
//!
//! That boundary is enforced by the compiler rather than by convention. The
//! browser SDK compiles this crate and nothing else; if a filesystem call could
//! be reached from here, it would link into the browser build and fail at
//! runtime instead of at review time. Reading a project *file* therefore lives
//! in [`auru-pm`](https://docs.rs/auru-pm), which adds the filesystem, the
//! providers, and the sync engine on top of this.
//!
//! What that leaves in scope:
//!
//! - **Commit identity** ([`canonical`]) — the RFC 8785 rule whose BLAKE3 is a
//!   commit's id. Providers verify it, so every client must reproduce it
//!   exactly; see `spec/auru-pm-v1.md` and `spec/vectors/`.
//! - **Project formats** ([`project_format`]) — reversible normalization of
//!   Ableton Live Sets, FL Studio projects, and DAWproject archives to and from
//!   canonical JSON.
//! - **Understanding a project** ([`ableton`], [`flstudio`], [`dawproject`],
//!   [`project_info`]) — tempo, key, tracks, plugins, and asset references.
//! - **Comparing versions** ([`diff`], [`merge`]).

pub mod ableton;
pub mod canonical;
pub mod commit;
pub mod dawproject;
pub mod diff;
pub mod error;
pub mod flstudio;
pub mod hash;
pub mod merge;
pub mod project_format;
pub mod project_info;
pub mod sample_manifest;

pub use ableton::{
    AbletonMetadata, AssetRef, AssetSummary, IntegrityProblem, KeyInfo, PluginFormat, PluginId,
    PluginRef, RefClass, TimeSignature, TrackCounts, TrackKind, TrackSummary,
};
pub use auru_pm_protocol::WIRE_VERSION;
pub use canonical::{canonical_encoding, compute_commit_id};
pub use commit::{AuthorIdentity, Commit, CommitId, CommitSummary, HistoryRange, TreeRef};
pub use dawproject::{
    DawprojectAssetRef, DawprojectAssetSummary, DawprojectMetadata, DawprojectTrackCounts,
    DawprojectTrackKind, DawprojectTrackSummary,
};
pub use diff::{
    ChangeKind, ChangeRow, ChangeTag, ChannelDiff, ChannelKind, ProjectDiff, structured_diff,
    summarize_diff,
};
pub use error::{Error, Result};
pub use hash::{ContentHash, ParseHashError};
pub use merge::{
    ConflictChoice, ConflictResolution, ConflictedField, MergeOutcome, merge3, merge3_json_bytes,
    resolve_conflicts,
};
pub use project_format::{ProjectFormat, ProjectSnapshot};
pub use project_info::{PROJECT_INFO_SCHEMA, ProjectInfo};
pub use sample_manifest::{AssetKind, SampleEntry, SampleManifest};

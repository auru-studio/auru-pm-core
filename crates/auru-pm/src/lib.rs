//! Git-like project history and pluggable sync providers
//! for native `.auru`, DAWproject, and Ableton Live Set projects.
//!
//! This crate is the platform half. The portable half —  commit identity,
//! project formats, diff, and merge — lives in [`auru_pm_kernel`] and is
//! re-exported here in full, so `auru_pm::Commit`, `auru_pm::structured_diff`
//! and friends resolve exactly as they always have. What this crate adds is
//! everything that needs a machine: the filesystem, content-addressed storage,
//! providers, OAuth, the OS keychain, and the sync engine.
//!
//! The split exists so a browser can run the kernel. See the `auru-pm-v1` HTTP
//! contract in [`spec/auru-pm-v1.md`](../../spec/auru-pm-v1.md).

pub mod ableton;
pub mod asset_plan;
pub mod cas;
pub mod discovery;
pub mod filesystem;
pub mod flstudio;
pub mod http;
pub mod oauth;
pub mod plugin_registry;
pub mod project_io;
pub mod provider;
pub mod registry;
pub mod sidecar;
pub mod sync;
pub mod token_store;
mod verified_io;

// The portable core, re-exported so downstream paths such as
// `auru_pm::commit::Commit` and `auru_pm::project_format::ProjectSnapshot`
// keep resolving after the split.
pub use auru_pm_kernel::{
    AbletonMetadata, AssetKind, AssetRef, AssetSummary, AuthorIdentity, ChangeKind, ChangeRow,
    ChangeTag, ChannelDiff, ChannelKind, Commit, CommitId, CommitSummary, ConflictChoice,
    ConflictResolution, ConflictedField, ContentHash, DawprojectAssetRef, DawprojectAssetSummary,
    DawprojectMetadata, DawprojectTrackCounts, DawprojectTrackKind, DawprojectTrackSummary, Error,
    HistoryRange, IntegrityProblem, KeyInfo, MergeOutcome, PROJECT_INFO_SCHEMA, ParseHashError,
    PluginFormat, PluginId, PluginRef, ProjectDiff, ProjectFormat, ProjectInfo, ProjectSnapshot,
    RefClass, Result, SampleEntry, SampleManifest, TimeSignature, TrackCounts, TrackKind,
    TrackSummary, TreeRef, canonical_encoding, compute_commit_id, merge3, merge3_json_bytes,
    resolve_conflicts, structured_diff, summarize_diff,
};
pub use auru_pm_kernel::{
    canonical, commit, dawproject, diff, error, hash, merge, project_format, project_info,
    sample_manifest,
};

pub use ableton::{AbletonBundle, AssetPlan, BundlePolicy, PathAlias, PlannedAsset, ScanOptions};
pub use asset_plan::plan_assets;
pub use auru_pm_protocol::WIRE_VERSION;
pub use cas::{Cas, GcReport, collect_reachable, collect_reachable_with_roots};
pub use discovery::{DiscoveredProject, read_headline};
pub use filesystem::FilesystemProvider;
pub use http::{HttpAccount, HttpProvider, ProviderHealth};
pub use oauth::{DeviceCodeResponse, OAuthProgress, start_device_flow, start_standard_oauth_flow};
pub use plugin_registry::{
    AURU_PLUGIN_REGISTRY_URL, PluginAvailability, PluginEntry, PluginRegistry, PluginSearchPaths,
    PluginSource, ResolvedPlugin,
};
pub use project_io::{hash_file, restore_project, restore_snapshot_to_path, snapshot_project};
pub use provider::{
    AuthMethod, Capabilities, HeadAdvance, Member, PermSet, ProjectLocation, ProjectMetadata,
    ProjectProfile, ProjectProvider, ProviderProject, RetentionReport, RetentionRoots,
    RetentionRule, UserId,
};
pub use registry::{
    AURU_REGISTRY_URL, RegistryAvailability, RegistryDocument, RegistryEntry,
    get_or_fetch as fetch_registry, resolve_endpoint,
};
pub use sidecar::{RemoteState, SIDECAR_SUFFIX, Sidecar, Stash, sidecar_path_for};
pub use sync::{
    MirrorResult, PushOptions, PushOutcome, discard_stash, drain_pending_pushes,
    fetch_project_info, push_with_conflict_resolutions, push_with_freshness_check,
    push_with_options, stashed_snapshot, verify_commit_copy,
};

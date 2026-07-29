//! Git-like project history and pluggable sync providers
//! for native `.auru`, DAWproject, and Ableton Live Set projects.
//!
//! The crate supplies the commit model, content-addressed storage, structural
//! merge and diff support, local and HTTP providers, and reversible adapters
//! for the supported project formats. See the `auru-pm-v1` HTTP contract in
//! [`spec.md`](./spec.md).

pub mod ableton;
pub mod canonical;
pub mod cas;
pub mod commit;
pub mod diff;
pub mod discovery;
pub mod error;
pub mod filesystem;
pub mod flstudio;
pub mod hash;
pub mod http;
pub mod merge;
pub mod oauth;
pub mod plugin_registry;
pub mod project_format;
pub mod project_info;
pub mod provider;
pub mod registry;
pub mod sample_manifest;
pub mod sidecar;
pub mod sync;
pub mod token_store;

pub use ableton::{
    AbletonBundle, AbletonMetadata, AssetPlan, AssetRef, AssetSummary, BundlePolicy,
    IntegrityProblem, KeyInfo, PathAlias, PlannedAsset, PluginFormat, PluginId, PluginRef,
    RefClass, ScanOptions, TimeSignature, TrackCounts, TrackKind, TrackSummary,
};
pub use auru_pm_protocol::WIRE_VERSION;
pub use canonical::{canonical_encoding, compute_commit_id};
pub use cas::{Cas, GcReport, collect_reachable, collect_reachable_with_roots};
pub use commit::{AuthorIdentity, Commit, CommitId, CommitSummary, HistoryRange, TreeRef};
pub use diff::{
    ChangeKind, ChangeRow, ChangeTag, ChannelDiff, ChannelKind, ProjectDiff, structured_diff,
    summarize_diff,
};
pub use discovery::{DiscoveredProject, read_headline};
pub use error::{Error, Result};
pub use filesystem::FilesystemProvider;
pub use hash::{ContentHash, ParseHashError};
pub use http::{HttpAccount, HttpProvider};
pub use merge::{
    ConflictChoice, ConflictResolution, ConflictedField, MergeOutcome, merge3, merge3_json_bytes,
    resolve_conflicts,
};
pub use oauth::{DeviceCodeResponse, OAuthProgress, start_device_flow};
pub use plugin_registry::{
    AURU_PLUGIN_REGISTRY_URL, PluginAvailability, PluginEntry, PluginRegistry, PluginSearchPaths,
    PluginSource, ResolvedPlugin,
};
pub use project_format::{ProjectFormat, ProjectSnapshot, restore_project, snapshot_project};
pub use project_info::{PROJECT_INFO_SCHEMA, ProjectInfo};
pub use provider::{
    AuthMethod, Capabilities, HeadAdvance, Member, PermSet, ProjectProfile, ProjectProvider,
    ProviderProject, RetentionReport, RetentionRoots, RetentionRule, UserId,
};
pub use registry::{
    AURU_REGISTRY_URL, RegistryAvailability, RegistryDocument, RegistryEntry,
    get_or_fetch as fetch_registry, resolve_endpoint,
};
pub use sample_manifest::{SampleEntry, SampleManifest};
pub use sidecar::{RemoteState, SIDECAR_SUFFIX, Sidecar, Stash, sidecar_path_for};
pub use sync::{
    MirrorResult, PushOutcome, discard_stash, drain_pending_pushes, fetch_project_info,
    push_with_conflict_resolutions, push_with_freshness_check, stashed_snapshot,
};

use serde::{Deserialize, Serialize};

use crate::hash::ContentHash;

/// Commit identifier — the blake3 of the commit's canonical encoding.
///
/// A `CommitId` is byte-identical to a [`ContentHash`]; the distinct
/// wrapper exists so trait signatures can't accidentally cross blob
/// hashes with commit hashes. The canonical encoder lands in M1 once
/// the local filesystem provider needs to compute IDs itself.
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommitId(pub ContentHash);

/// Reference to a commit's tree — the snapshot blob plus a sample manifest.
///
/// `snapshot` is the blob containing canonical project JSON for this commit.
/// Native Auru projects use their normal JSON shape; external DAWs use
/// [`crate::ProjectSnapshot`]'s reversible normalized representation.
/// `samples` is the blob listing `(path, sample_hash)` pairs the project
/// depends on; samples are downloaded lazily via the content-addressed blob
/// store, so the manifest is the cheap "what do I need before playback" probe.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TreeRef {
    pub snapshot: ContentHash,
    pub samples: ContentHash,
}

/// Display identity captured at commit time.
///
/// Captured inline (rather than looked up via [`crate::Member`]) so the
/// history UI renders without a live provider connection and so author
/// attribution survives if a user is removed from a workspace later.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthorIdentity {
    pub display_name: String,
    pub provider_user_id: String,
    pub provider_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// A single commit in the project history.
///
/// `parents` length is the commit shape: 0 = root, 1 = normal, 2 = merge.
/// `id` is the blake3 of the canonical encoding of every other field in a
/// stable order — the canonical encoder is M1 work and lives next to the
/// filesystem provider.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Commit {
    pub id: CommitId,
    pub parents: Vec<CommitId>,
    pub tree: TreeRef,
    pub author: AuthorIdentity,
    /// Unix epoch seconds.
    pub timestamp: i64,
    /// One-line "what's changed" summary entered at Save Version time.
    pub message: String,
    /// Free-form description body. May be empty.
    #[serde(default)]
    pub description: String,
    /// Auru desktop version that produced the commit (`CARGO_PKG_VERSION`).
    pub auru_version: String,
    /// Source project or external-snapshot schema version at commit time. The
    /// history UI uses this to know whether a snapshot needs migration before
    /// restore.
    pub format_version: u32,
}

/// Trimmed commit row used by the flat history UI.
///
/// Omits [`TreeRef`] so listing history doesn't force a tree fetch per row.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommitSummary {
    pub id: CommitId,
    pub parents: Vec<CommitId>,
    pub author: AuthorIdentity,
    pub timestamp: i64,
    pub message: String,
    #[serde(default)]
    pub description: String,
}

impl From<&Commit> for CommitSummary {
    fn from(c: &Commit) -> Self {
        CommitSummary {
            id: c.id,
            parents: c.parents.clone(),
            author: c.author.clone(),
            timestamp: c.timestamp,
            message: c.message.clone(),
            description: c.description.clone(),
        }
    }
}

/// Pagination window for [`crate::ProjectProvider::list_history`].
///
/// Empty default = "from HEAD, provider-default page size".
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HistoryRange {
    /// Max rows to return. Providers may cap this at a sane upper bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Return commits strictly older than this id. `None` starts at HEAD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<CommitId>,
}

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::commit::{Commit, CommitId, CommitSummary, HistoryRange};
use crate::error::{Error, Result};
use crate::hash::ContentHash;

/// What a provider can do beyond the dumb-core surface.
///
/// The "dumb core" — blob CAS, commit log, HEAD pointer — is always
/// available on every provider. Anything richer (members, permissions,
/// branches, server-side merge) is feature-gated here, and the matching
/// trait method returns [`Error::Unsupported`] when the flag is `false`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Capabilities {
    pub members: bool,
    pub permissions: bool,
    pub branches: bool,
    pub server_side_merge: bool,
    pub auth_methods: Vec<AuthMethod>,
}

/// Authentication scheme advertised by a provider via `/v1/health`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    /// OAuth 2.0 device-code flow (used by the Auru-hosted reference
    /// provider — no embedded browser required).
    OAuthDeviceCode,
    /// Personal access token, pasted by the user into the Add Custom URL
    /// dialog and stored in the OS keychain.
    Pat,
    /// No authentication. Local filesystem provider, or trusted intranet.
    None,
}

/// Provider-scoped user identifier. Treated as opaque by callers; only
/// providers themselves give it meaning.
pub type UserId = String;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Member {
    pub user_id: UserId,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// Coarse permission set per user.
///
/// Deliberately coarse for the v1 teams-readiness hook — the full
/// roles-and-ACL model lives in the future teams plan. Providers that
/// don't ship a permissions story should report
/// `can_read = can_write = true, can_admin = false` for every member.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermSet {
    pub can_read: bool,
    pub can_write: bool,
    pub can_admin: bool,
}

/// Result of a compare-and-swap HEAD advance.
///
/// `Conflict` carries the provider's current HEAD (if any) so callers can
/// immediately re-merge against it without an extra `get_head` round-trip.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "result")]
pub enum HeadAdvance {
    Advanced,
    Conflict {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current: Option<CommitId>,
    },
}

/// The trait every project provider implements.
///
/// Required methods make up the dumb-core CAS + commit log surface that
/// every provider — bundled filesystem, HTTP `auru-pm-v1`, the Auru-
/// hosted reference, custom user URLs — must satisfy. Optional methods
/// have default impls that return [`Error::Unsupported`] so providers
/// that don't advertise the capability don't need to override them.
///
/// `Send + Sync` is required so providers are usable behind
/// `Arc<dyn ProjectProvider>` from the async push fan-out.
#[async_trait]
pub trait ProjectProvider: Send + Sync {
    fn capabilities(&self) -> Capabilities;

    // -- Content-addressed blob store ----------------------------------

    /// Upload a blob keyed by its blake3 hash. Idempotent — providers
    /// must accept and silently no-op on a re-upload of an existing hash.
    async fn put_blob(&self, hash: &ContentHash, bytes: &[u8]) -> Result<()>;

    /// Probe: for each input hash, is the blob already present?
    /// Returns parallel-indexed booleans, same length as `hashes`.
    async fn has_blobs(&self, hashes: &[ContentHash]) -> Result<Vec<bool>>;

    async fn get_blob(&self, hash: &ContentHash) -> Result<Vec<u8>>;

    // -- Commit log ----------------------------------------------------

    /// Store a commit. The provider MUST verify `commit.id` matches the
    /// canonical encoding of the rest of the fields; mismatches are an
    /// auth-equivalent failure (clients writing what they didn't compute).
    async fn put_commit(&self, commit: &Commit) -> Result<CommitId>;

    async fn get_commit(&self, id: &CommitId) -> Result<Commit>;

    async fn list_history(&self, range: HistoryRange) -> Result<Vec<CommitSummary>>;

    // -- HEAD pointer --------------------------------------------------

    async fn get_head(&self) -> Result<Option<CommitId>>;

    /// Compare-and-swap HEAD from `from` to `to`. `from == None` is the
    /// "initial publish" case (no prior HEAD). Returns
    /// [`HeadAdvance::Conflict`] without modifying state if the actual
    /// current HEAD differs from `from`.
    async fn advance_head(&self, from: Option<CommitId>, to: CommitId) -> Result<HeadAdvance>;

    // -- Optional, capability-gated ------------------------------------
    //
    // ╔═══ TEAMS INTEGRATION BOUNDARY ═════════════════════════════════╗
    // The two methods below are the M6 teams-readiness hooks. They give
    // the future Auru teams feature (roles, invites, ACL UX) a stable
    // trait-level surface that already exists on every provider, gated
    // by `Capabilities::members` / `Capabilities::permissions`.
    //
    // Live-collab handoff: the `Member.user_id` returned here is the
    // same identity space as `auru_session::PeerId` — the session crate
    // is the integration point for presence (who's currently editing).
    // A teams-aware UI will reconcile `list_members()` (the team roster)
    // with `auru_session`'s connected-peer list (who's online right now)
    // and `permissions()` (what each peer is allowed to do).
    //
    // Providers that don't ship a teams story keep the default
    // `Unsupported` impls and the UI degrades to a single-author flow.
    // ╚═══════════════════════════════════════════════════════════════╝

    /// Available iff `capabilities().members`.
    ///
    /// Returns the team roster. The future teams UI consumes this for
    /// invite management and to render an "active member" badge next to
    /// commits whose `AuthorIdentity.provider_user_id` matches a member.
    async fn list_members(&self) -> Result<Vec<Member>> {
        Err(Error::Unsupported("members"))
    }

    /// Available iff `capabilities().permissions`.
    ///
    /// Returns the permission set for a single user. Coarse-grained on
    /// purpose; the full roles-and-ACL design lives in a later teams
    /// plan. UI callers should treat `Unsupported` as "everyone can do
    /// everything" — the same shape the filesystem provider reports.
    async fn permissions(&self, _user: &UserId) -> Result<PermSet> {
        Err(Error::Unsupported("permissions"))
    }
}

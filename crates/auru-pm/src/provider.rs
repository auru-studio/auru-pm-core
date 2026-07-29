use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::commit::{Commit, CommitId, CommitSummary, HistoryRange};
use crate::error::{Error, Result};
use crate::hash::ContentHash;
use crate::project_format::ProjectFormat;

/// Metadata registered for a project so an account-level catalogue can show
/// it without downloading the latest snapshot.
pub type ProjectProfile = auru_pm_protocol::ProjectProfile<ProjectFormat>;

/// One project returned by a provider account catalogue.
pub type ProviderProject = auru_pm_protocol::ProviderProject<CommitId, ProjectFormat>;

/// Destructive history policy applied by a provider.
pub type RetentionRule = auru_pm_protocol::RetentionRule;

/// Result of applying a [`RetentionRule`].
pub type RetentionReport = auru_pm_protocol::RetentionReport;

/// Provider objects protected from retention by active client workflows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RetentionRoots {
    pub commits: Vec<CommitId>,
    pub blobs: Vec<ContentHash>,
}

/// What a provider can do beyond the dumb-core surface.
///
/// The "dumb core" — blob CAS, commit log, HEAD pointer — is always
/// available on every provider. Anything richer (members, permissions,
/// branches, server-side merge) is feature-gated here, and the matching
/// trait method returns [`Error::Unsupported`] when the flag is `false`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Capabilities {
    /// Whether the provider implements account-level project discovery.
    #[serde(default)]
    pub project_listing: bool,
    pub members: bool,
    pub permissions: bool,
    pub branches: bool,
    pub server_side_merge: bool,
    pub auth_methods: Vec<AuthMethod>,
    /// Whether the server decodes `Content-Encoding: gzip` on blob uploads.
    ///
    /// Defaults to `false`, which is what makes this safe to add: a server
    /// written before this field existed omits it, the client reads `false`,
    /// and uploads stay uncompressed. Sending a compressed body to a server
    /// that ignored the header would have it store the compressed bytes under
    /// the plaintext hash — corruption that would only surface later, when the
    /// blob was read back and no longer parsed.
    ///
    /// Downloads need no capability: response compression is negotiated by
    /// `Accept-Encoding` in the ordinary way.
    #[serde(default)]
    pub compressed_uploads: bool,
    /// Whether the provider can enforce destructive history-retention rules.
    #[serde(default)]
    pub history_retention: bool,
    /// Whether blob operations are authorized through the project namespace.
    ///
    /// `false` preserves compatibility with servers predating private
    /// per-project blob entitlements, which exposed `/v1/blobs/*`.
    #[serde(default)]
    pub project_scoped_blobs: bool,
}

/// Authentication scheme advertised by a provider via `/v1/health`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    /// OAuth 2.0 Authorization Code grant for a public desktop client, with
    /// PKCE S256 and an exact loopback redirect URI.
    #[serde(rename = "authorization_code_pkce")]
    OAuthAuthorizationCodePkce,
    /// OAuth 2.0 device-code flow (used by the Auru-hosted reference
    /// provider — no embedded browser required).
    ///
    /// Named explicitly because `rename_all = "snake_case"` turns `OAuth`
    /// into `o_auth`, which is not what [`spec.md`](../spec.md) documents and
    /// not what a server implementing it would send. The alias keeps anything
    /// written with the derived spelling readable.
    #[serde(rename = "oauth_device_code", alias = "o_auth_device_code")]
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

    /// Store the human-facing metadata used by account-level project lists.
    ///
    /// Available iff `capabilities().project_listing`. Providers should treat
    /// this as an idempotent upsert for the current project handle.
    async fn put_project_profile(&self, _profile: &ProjectProfile) -> Result<()> {
        Err(Error::Unsupported("project listing"))
    }

    /// Permanently remove history older than `rule` permits.
    ///
    /// Available iff `capabilities().history_retention`. HEAD is never
    /// removed. Implementations may retain newly orphaned objects for a grace
    /// period, but removed versions must disappear from [`Self::list_history`]
    /// before this call returns. `protected` carries queued or stashed client
    /// work that must survive even when it falls outside that visible history.
    async fn prune_history(
        &self,
        _rule: RetentionRule,
        _protected: &RetentionRoots,
    ) -> Result<RetentionReport> {
        Err(Error::Unsupported("history retention"))
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_methods_should_use_the_names_the_spec_documents() {
        assert_eq!(
            serde_json::to_string(&AuthMethod::OAuthAuthorizationCodePkce).expect("encode"),
            r#""authorization_code_pkce""#
        );
        // `spec.md` and the OAuth module both say `oauth_device_code`. Serde's
        // snake_case derive would have produced `o_auth_device_code`, so a
        // server implementing the published protocol could not be parsed.
        let json = serde_json::to_string(&AuthMethod::OAuthDeviceCode).expect("encode");
        assert_eq!(json, r#""oauth_device_code""#);

        assert_eq!(
            serde_json::to_string(&AuthMethod::Pat).expect("encode"),
            r#""pat""#
        );
        assert_eq!(
            serde_json::to_string(&AuthMethod::None).expect("encode"),
            r#""none""#
        );
    }

    #[test]
    fn the_derived_spelling_should_still_be_accepted() {
        // Anything already written with the accidental name keeps working.
        let decoded: AuthMethod =
            serde_json::from_str(r#""o_auth_device_code""#).expect("decode legacy spelling");
        assert_eq!(decoded, AuthMethod::OAuthDeviceCode);
    }

    #[test]
    fn capabilities_should_round_trip_through_the_health_shape() {
        let capabilities = Capabilities {
            auth_methods: vec![AuthMethod::OAuthDeviceCode, AuthMethod::Pat],
            ..Capabilities::default()
        };
        let json = serde_json::to_string(&capabilities).expect("encode");
        assert!(json.contains("oauth_device_code"), "{json}");

        let decoded: Capabilities = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded.auth_methods, capabilities.auth_methods);
    }
}

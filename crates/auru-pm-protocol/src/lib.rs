//! Transport types shared by Auru PM clients and servers.

use serde::{Deserialize, Serialize};

/// HTTP protocol version implemented by this workspace.
pub const WIRE_VERSION: &str = "auru-pm-v1";

/// Response returned by `GET /v1/health`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthResponse<C> {
    pub protocol: String,
    /// Stable identifier used in commit author identities and sidecars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub capabilities: C,
    /// Standards-based OAuth/OIDC settings safe to publish to desktop clients.
    ///
    /// Absent for unauthenticated providers and legacy providers whose
    /// authentication flow is described only by `capabilities.auth_methods`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication: Option<OAuthClientConfiguration>,
}

/// Public OAuth configuration for one PM server.
///
/// Endpoint URLs are deliberately absent: clients discover them from the
/// issuer's RFC 8414 / OpenID Connect metadata instead of trusting duplicated
/// configuration.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthClientConfiguration {
    pub issuer: String,
    pub audience: String,
    pub client_id: String,
    pub required_scope: String,
    pub redirect_uri: String,
    pub flows: Vec<OAuthFlow>,
}

/// OAuth grants a provider permits its public desktop client to use.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OAuthFlow {
    #[serde(rename = "authorization_code_pkce")]
    AuthorizationCodePkce,
    DeviceAuthorization,
}

/// Identity derived by the provider from a verified bearer token.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthenticatedIdentity {
    pub provider_id: String,
    pub user_id: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// Response containing a project's current commit.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeadResponse<I> {
    pub commit_id: Option<I>,
}

/// Compare-and-swap request for advancing a project head.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdvanceHeadRequest<I> {
    pub from: Option<I>,
    pub to: I,
}

/// Conflict response returned when the expected head is stale.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConflictResponse<I> {
    #[serde(default)]
    pub current: Option<I>,
}

/// Response returned after storing a commit.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PutCommitResponse<I> {
    pub id: I,
}

/// Ordered commit history response.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryResponse<C> {
    pub commits: Vec<C>,
}

/// Destructive history policy applied by a provider.
///
/// There is deliberately no "everything" variant: keeping everything means
/// not calling the retention endpoint. Once a provider has removed a version,
/// changing the desktop setting cannot resurrect it.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "policy")]
pub enum RetentionRule {
    /// Keep the newest `count` versions. Providers always keep HEAD, including
    /// when a malformed client sends `count: 0`.
    Latest { count: u32 },
    /// Keep HEAD and the connected history prefix through the oldest commit at
    /// or after this Unix timestamp.
    Since { timestamp: i64 },
}

/// Retention request plus objects that active client workflows still need.
///
/// Providers must preserve these roots even when they sit outside the visible
/// history boundary. Pending mirror pushes and pre-merge stashes are the two
/// ordinary examples.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetentionRequest<I, H> {
    pub rule: RetentionRule,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protected_commits: Vec<I>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protected_blobs: Vec<H>,
}

impl RetentionRule {
    /// Number of entries to keep from a newest-first linear history.
    pub fn retained_prefix_len(self, timestamps: impl IntoIterator<Item = i64>) -> usize {
        let timestamps: Vec<i64> = timestamps.into_iter().collect();
        if timestamps.is_empty() {
            return 0;
        }
        match self {
            Self::Latest { count } => (count.max(1) as usize).min(timestamps.len()),
            Self::Since { timestamp } => timestamps
                .iter()
                .rposition(|candidate| *candidate >= timestamp)
                .map_or(1, |index| index + 1),
        }
    }
}

/// Result of applying a [`RetentionRule`].
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetentionReport {
    /// Versions that disappeared from this project's visible history.
    pub versions_removed: u64,
    /// Content-addressed objects physically reclaimed during this pass.
    ///
    /// Providers may keep recently orphaned objects for a grace period, so
    /// this can be zero even when `versions_removed` is non-zero.
    pub objects_removed: u64,
    /// Physical encoded bytes reclaimed during this pass.
    pub bytes_freed: u64,
}

/// Human-facing metadata registered for one provider-scoped project handle.
///
/// The provider cannot infer either value from an opaque handle, and listing
/// projects should not require downloading every project's latest snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectProfile<F> {
    pub display_name: String,
    pub format: F,
}

/// One project visible to the authenticated provider account.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderProject<I, F> {
    pub handle: String,
    pub head: I,
    /// Absent for projects written by clients predating project catalogues.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<ProjectProfile<F>>,
    /// Timestamp of the HEAD commit, in Unix epoch seconds.
    pub updated_at: i64,
}

/// Response returned by `GET /v1/projects`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectsResponse<I, F> {
    pub projects: Vec<ProviderProject<I, F>>,
}

/// Batch request asking which content hashes already exist.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HasBlobsRequest<H> {
    pub hashes: Vec<H>,
}

/// Parallel response for [`HasBlobsRequest`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HasBlobsResponse {
    pub present: Vec<bool>,
}

/// Error body returned by the HTTP server.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorResponse {
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_legacy_project_without_a_profile_should_still_decode() {
        let response: ProjectsResponse<String, String> = serde_json::from_str(
            r#"{"projects":[{"handle":"song","head":"commit","updated_at":1750000000}]}"#,
        )
        .expect("project list");

        assert_eq!(response.projects.len(), 1);
        assert_eq!(response.projects[0].handle, "song");
        assert!(response.projects[0].profile.is_none());
    }

    #[test]
    fn retention_rules_should_keep_a_connected_newest_first_prefix() {
        let timestamps = [300, 200, 100];

        assert_eq!(
            RetentionRule::Latest { count: 2 }.retained_prefix_len(timestamps),
            2
        );
        assert_eq!(
            RetentionRule::Latest { count: 0 }.retained_prefix_len(timestamps),
            1,
            "HEAD can never be removed"
        );
        assert_eq!(
            RetentionRule::Since { timestamp: 150 }.retained_prefix_len(timestamps),
            2
        );
        assert_eq!(
            RetentionRule::Since { timestamp: 999 }.retained_prefix_len(timestamps),
            1,
            "HEAD survives even when every commit predates the cutoff"
        );
    }

    #[test]
    fn oauth_health_metadata_should_round_trip_and_remain_optional() {
        let configured = HealthResponse {
            protocol: WIRE_VERSION.to_owned(),
            provider_id: Some("studio-pm".to_owned()),
            name: Some("Studio PM".to_owned()),
            capabilities: serde_json::json!({}),
            authentication: Some(OAuthClientConfiguration {
                issuer: "https://auth.example.com".to_owned(),
                audience: "auru-pm".to_owned(),
                client_id: "auru-desktop".to_owned(),
                required_scope: "openid".to_owned(),
                redirect_uri: "http://127.0.0.1:43827/oauth/callback".to_owned(),
                flows: vec![
                    OAuthFlow::AuthorizationCodePkce,
                    OAuthFlow::DeviceAuthorization,
                ],
            }),
        };
        let encoded = serde_json::to_string(&configured).expect("health response");
        let decoded: HealthResponse<serde_json::Value> =
            serde_json::from_str(&encoded).expect("decode health response");
        assert_eq!(decoded, configured);

        let legacy: HealthResponse<serde_json::Value> =
            serde_json::from_str(r#"{"protocol":"auru-pm-v1","capabilities":{}}"#)
                .expect("legacy health response");
        assert!(legacy.authentication.is_none());
    }
}

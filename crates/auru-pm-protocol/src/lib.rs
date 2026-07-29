//! Transport types shared by Auru PM clients and servers.

use serde::{Deserialize, Serialize};

/// HTTP protocol version implemented by this workspace.
pub const WIRE_VERSION: &str = "auru-pm-v1";

/// Response returned by `GET /v1/health`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthResponse<C> {
    pub protocol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub capabilities: C,
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
}

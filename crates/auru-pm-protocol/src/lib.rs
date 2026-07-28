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

use thiserror::Error;

/// Error returned by [`crate::ProjectProvider`] methods.
///
/// The variants cover the boundaries any provider implementation will
/// cross — network, auth, capability, content-store. Variants are
/// intentionally coarse: callers branch on category, not specific
/// HTTP status codes, so the trait stays equally usable for the
/// bundled filesystem provider and the HTTP client.
#[derive(Debug, Error)]
pub enum Error {
    #[error("network: {0}")]
    Network(String),

    #[error("auth: {0}")]
    Auth(String),

    #[error("not found: {0}")]
    NotFound(String),

    /// `advance_head` lost a compare-and-swap race against the remote.
    /// Callers should refetch HEAD and merge before retrying.
    #[error("conflict: HEAD moved")]
    HeadConflict,

    /// Local and remote diverged and the automatic merge found fields both
    /// sides changed to different values. The caller must resolve each conflict
    /// before retrying.
    #[error("merge conflict: {count} field(s) could not be auto-merged")]
    MergeConflict { count: usize },

    /// The provider does not advertise the capability required to serve
    /// this call. Methods guarded by [`crate::Capabilities`] flags return
    /// this by default.
    #[error("unsupported capability: {0}")]
    Unsupported(&'static str),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    /// A project file could not be detected, decoded, or reconstructed.
    #[error("project format: {0}")]
    ProjectFormat(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

//! Client-facing API for connecting applications to Auru PM providers.
//!
//! [`ProviderAccount`] is the account-level seam: it lists projects before a
//! caller has a project handle, then opens the ordinary project-scoped
//! [`ProjectProvider`] used by backup, history and restore.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use auru_pm::{Error, FilesystemProvider, HttpAccount, ProjectProvider, ProviderProject, Result};

/// A connected provider account, before any one project has been selected.
#[derive(Clone, Debug)]
pub enum ProviderAccount {
    Http {
        account: HttpAccount,
    },
    Filesystem {
        /// Parent containing one repository directory per project handle.
        projects_root: PathBuf,
    },
}

impl ProviderAccount {
    /// Connect an HTTP provider account and cache its advertised capabilities.
    pub async fn connect_http(endpoint: &str, token: Option<String>) -> Result<Self> {
        Ok(Self::Http {
            account: HttpAccount::connect(endpoint, token).await?,
        })
    }

    pub fn filesystem(projects_root: impl Into<PathBuf>) -> Self {
        Self::Filesystem {
            projects_root: projects_root.into(),
        }
    }

    /// Whether this account can enumerate projects before a handle is known.
    pub fn supports_project_listing(&self) -> bool {
        match self {
            Self::Http { account } => account.capabilities().project_listing,
            Self::Filesystem { .. } => true,
        }
    }

    /// List projects visible to this provider account, newest first.
    pub async fn list_projects(&self) -> Result<Vec<ProviderProject>> {
        match self {
            Self::Http { account } => account.list_projects().await,
            Self::Filesystem { projects_root } => FilesystemProvider::list_projects(projects_root),
        }
    }

    /// Open one project-scoped provider selected from [`Self::list_projects`].
    pub async fn open_project(&self, handle: &str) -> Result<Arc<dyn ProjectProvider>> {
        match self {
            Self::Http { account } => Ok(Arc::new(account.open_project(handle))),
            Self::Filesystem { projects_root } => Ok(Arc::new(FilesystemProvider::open(
                filesystem_project_path(projects_root, handle)?,
            )?)),
        }
    }
}

fn filesystem_project_path(root: &Path, handle: &str) -> Result<PathBuf> {
    let mut components = Path::new(handle).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(root.join(handle)),
        _ => Err(Error::Other(
            "filesystem project handles must be one safe path segment".to_owned(),
        )),
    }
}

pub use auru_pm::token_store::{
    delete_provider_token, delete_token, load_provider_token, load_token, store_provider_token,
    store_token,
};
pub use auru_pm::{
    DeviceCodeResponse, OAuthProgress, RegistryEntry, fetch_registry, resolve_endpoint,
    start_device_flow,
};
pub use auru_pm_protocol::WIRE_VERSION;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filesystem_handles_must_not_escape_the_projects_root() {
        let root = Path::new("/backups/projects");
        assert_eq!(
            filesystem_project_path(root, "night-drive").unwrap(),
            root.join("night-drive")
        );
        assert!(filesystem_project_path(root, "../elsewhere").is_err());
        assert!(filesystem_project_path(root, "/absolute").is_err());
    }
}

//! Per-user PM state stored alongside the `.auru` project file as
//! `<project>.auru-pm.json`.
//!
//! Holds everything that's specific to one machine + one user: which
//! provider is primary, the local HEAD pointer, per-remote sync state,
//! the queue of pushes pending while the network was down. Tokens
//! never go in here — those live in the OS keychain keyed by
//! `(provider_id, project_id)`. The file is intended to be gitignored
//! by anyone collaborating on the project at the source level.
//!
//! Writes are atomic via a `<file>.tmp` + rename, matching
//! [`crate::cas`]. Two processes racing on the same project would still
//! conflict at the application layer; for now we assume Auru desktop
//! is the only writer per host.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::commit::CommitId;
use crate::error::{Error, Result};
use crate::hash::ContentHash;
use crate::provider::{ProjectLocation, ProjectMetadata};

/// Conventional filename suffix appended to the `.auru` path.
pub const SIDECAR_SUFFIX: &str = ".auru-pm.json";

/// Compute the sidecar path for an `.auru` project file at `project_path`.
///
/// Convention: `foo.auru` → `foo.auru-pm.json`. The PM file lives
/// directly next to the project so opening a project off, say, a USB
/// stick carries the local HEAD with it.
pub fn sidecar_path_for(project_path: &Path) -> PathBuf {
    let mut name = project_path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    name.push("-pm.json");
    // `.auru` → `.auru-pm.json`; if the extension is missing entirely,
    // just append `-pm.json` to whatever was there.
    project_path.with_file_name(name)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Sidecar {
    /// Portable placement beneath the watched library root. This is mirrored
    /// into the provider profile so recovery can rebuild the same organizing
    /// folders without recording an absolute path from this computer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<ProjectLocation>,

    /// Mutable project labels available before a provider has been selected and
    /// mirrored into the provider profile once the project is backed up.
    #[serde(default, skip_serializing_if = "ProjectMetadata::is_empty")]
    pub metadata: ProjectMetadata,

    /// Provider id of the designated primary, eg `"auru-hosted"` or
    /// `"local-folder://..."`. `None` when no provider is enrolled yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<String>,

    /// Provider-scoped opaque project handles, keyed by provider id.
    ///
    /// A handle must survive local folder renames and mount-point changes.
    /// Keeping it beside the project also means a project and its PM state can
    /// move to another machine together without opening a second remote
    /// history under a path-derived name.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub provider_handles: std::collections::BTreeMap<String, String>,

    /// The commit id the local working state was last synced to.
    /// Compared against the primary's HEAD on open to decide whether
    /// a pull is needed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_head: Option<CommitId>,

    /// Most recent commit whose provider objects and source dependencies were
    /// both verified as complete. This is intentionally separate from
    /// `local_head`: a commit may exist while still omitting an unreadable
    /// sample, and that state must never be shown as safe to delete locally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_head: Option<CommitId>,

    /// Per-remote-provider sync state, keyed by provider id.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub remotes: std::collections::BTreeMap<String, RemoteState>,

    /// Commits that succeeded locally but haven't been pushed to every
    /// configured provider — drained on reconnect.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_pushes: Vec<CommitId>,

    /// The working state as it was before the last merge attempt.
    ///
    /// Set whenever a push has to reconcile with a remote that moved, and
    /// cleared once the resulting commit lands. While it is set, the user's
    /// own version of the project is recoverable in full regardless of what
    /// the merge did. See [`Stash`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stash: Option<Stash>,
}

/// A snapshot of local work taken before a merge.
///
/// Merging someone else's changes into a project is the one operation here
/// that can leave a person worse off than before they started. The snapshot
/// costs one content-addressed blob — usually already stored, since the same
/// bytes are about to be pushed — and turns every merge into something that
/// can be walked back.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Stash {
    /// CAS hash of the local snapshot as it stood before merging.
    pub snapshot: ContentHash,
    /// DAWproject archive resources detached from a version-two snapshot.
    ///
    /// Keeping these hashes beside the snapshot makes a stash independently
    /// restorable after restart and lets retention protect every required
    /// object until the stash is accepted or discarded.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resources: BTreeMap<String, ContentHash>,
    /// Unix epoch seconds the stash was taken.
    pub created_at: i64,
    /// Commit the local work was based on, for context when restoring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<CommitId>,
    /// Why the stash was taken, shown when offering to restore it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_head: Option<CommitId>,
    /// Unix epoch seconds when we last successfully pulled this remote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_pulled: Option<i64>,
}

impl Sidecar {
    /// Load the sidecar at `path`. Returns `Ok(default)` if the file
    /// doesn't exist yet — that's the normal case before "Enable PM"
    /// has been run for this project on this machine.
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(s) => Ok(serde_json::from_str(&s)?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(Error::Io(e)),
        }
    }

    /// Atomically write the sidecar to `path`. Pretty-printed so users
    /// who go poking around can read it; the file is small (a few
    /// kilobytes at most) so the cost is negligible.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_vec_pretty(self)?;
        let tmp = with_tmp_suffix(path);
        fs::write(&tmp, &body)?;
        match fs::rename(&tmp, path) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                Err(Error::Io(e))
            }
        }
    }

    /// Convenience: load, mutate via `f`, write back. Single-process,
    /// so no locking — see module docs.
    pub fn modify(path: &Path, f: impl FnOnce(&mut Sidecar)) -> Result<Sidecar> {
        let mut sidecar = Self::load(path)?;
        f(&mut sidecar);
        sidecar.save(path)?;
        Ok(sidecar)
    }
}

fn with_tmp_suffix(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::ContentHash;
    use tempfile::TempDir;

    #[test]
    fn sidecar_path_appends_suffix() {
        let p = Path::new("/tmp/song.auru");
        assert_eq!(sidecar_path_for(p), Path::new("/tmp/song.auru-pm.json"));
    }

    #[test]
    fn load_missing_returns_default() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("song.auru-pm.json");
        let sidecar = Sidecar::load(&path).unwrap();
        assert_eq!(sidecar, Sidecar::default());
    }

    #[test]
    fn save_then_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("song.auru-pm.json");
        let head = CommitId(ContentHash::of(b"head"));
        let remote = CommitId(ContentHash::of(b"remote"));

        let mut sidecar = Sidecar {
            location: Some(ProjectLocation {
                relative_path: "Ableton/Projects/Night Drive Project".into(),
            }),
            metadata: ProjectMetadata {
                genre: Some("Ambient".into()),
                tags: vec!["finished".into(), "live set".into()],
            },
            primary: Some("local-folder://foo".into()),
            provider_handles: std::collections::BTreeMap::from([(
                "local-folder://foo".into(),
                "opaque-project-handle".into(),
            )]),
            local_head: Some(head),
            ..Sidecar::default()
        };
        sidecar.remotes.insert(
            "local-folder://foo".into(),
            RemoteState {
                remote_head: Some(remote),
                last_pulled: Some(1_700_000_000),
            },
        );
        sidecar.pending_pushes.push(head);
        sidecar.save(&path).unwrap();

        let loaded = Sidecar::load(&path).unwrap();
        assert_eq!(loaded, sidecar);
        assert_eq!(
            loaded.provider_handles.get("local-folder://foo"),
            Some(&"opaque-project-handle".to_owned())
        );
        assert_eq!(loaded.metadata.genre.as_deref(), Some("Ambient"));
        assert_eq!(
            loaded
                .location
                .as_ref()
                .map(|location| location.relative_path.as_str()),
            Some("Ableton/Projects/Night Drive Project")
        );
    }

    #[test]
    fn empty_sidecar_serializes_compactly() {
        // Default sidecar should serialize as `{}` — no stray
        // `"remotes": {}` or `"pending_pushes": []` keys cluttering
        // the file.
        let body = serde_json::to_string(&Sidecar::default()).unwrap();
        assert_eq!(body, "{}");
    }

    #[test]
    fn modify_mutates_and_persists() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("song.auru-pm.json");
        let head = CommitId(ContentHash::of(b"head"));
        Sidecar::modify(&path, |s| {
            s.local_head = Some(head);
            s.primary = Some("local-folder://x".into());
        })
        .unwrap();
        let loaded = Sidecar::load(&path).unwrap();
        assert_eq!(loaded.local_head, Some(head));
        assert_eq!(loaded.primary.as_deref(), Some("local-folder://x"));
    }
}

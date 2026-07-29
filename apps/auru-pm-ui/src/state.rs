//! What Auru remembers between launches.
//!
//! Small and deliberately boring: which folders to look in, which projects
//! were added by hand, and the handful of preferences the settings window
//! writes. Everything else — a project's tempo, whether it is backed up, what
//! its history contains — is read from the projects themselves and from their
//! sidecars, because those are the truth and a cache of them would only be a
//! second thing that can be wrong.
//!
//! Nothing here is a credential. Provider tokens live in the operating
//! system's keychain and never reach this file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Bumped when the shape changes in a way older builds cannot read.
pub const STATE_SCHEMA: u32 = 1;

const fn existing_profile_completed_onboarding() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct FileRevision {
    unix_seconds: u64,
    nanoseconds: u32,
}

impl FileRevision {
    fn from_system_time(time: SystemTime) -> Option<Self> {
        let elapsed = time.duration_since(UNIX_EPOCH).ok()?;
        Some(Self {
            unix_seconds: elapsed.as_secs(),
            nanoseconds: elapsed.subsec_nanos(),
        })
    }

    fn to_system_time(self) -> Option<SystemTime> {
        if self.nanoseconds >= 1_000_000_000 {
            return None;
        }
        UNIX_EPOCH.checked_add(std::time::Duration::new(
            self.unix_seconds,
            self.nanoseconds,
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PathAliasState {
    pub from: String,
    pub to: PathBuf,
}

/// Everything Auru carries from one launch to the next.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AppState {
    /// Where this state was read from, and the only place it will be written.
    ///
    /// Absent on a state built in memory, which makes [`Self::save`] a no-op.
    /// That is deliberate: `load_library` persists first-seen times, so without
    /// this a test constructing a throwaway `AppState` would overwrite the
    /// real user's settings — which is exactly what happened.
    #[serde(skip)]
    origin: Option<PathBuf>,
    pub schema: u32,
    /// Name shown against saved versions.
    pub display_name: String,
    /// Whether the profile has completed the current setup flow.
    ///
    /// Missing means true for state written by older builds, which had already
    /// treated choosing a display name as completed onboarding.
    #[serde(default = "existing_profile_completed_onboarding")]
    pub onboarding_complete: bool,
    /// `"night"` or `"day"`.
    pub appearance: String,
    pub automatic_backups: bool,
    pub verify_uploads: bool,
    /// Key of the chosen retention option.
    pub version_retention: String,
    /// Key of the sidebar's sort order. See `model::SortOrder::from_key`.
    pub sort_order: String,
    /// Unix seconds when Auru first saw each project, keyed by its folder.
    ///
    /// The only way to answer "recently added", because nothing on disk
    /// records it — a project folder is as old as the music inside it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub first_seen: BTreeMap<String, i64>,
    /// Exact project revisions already handed to the backup coordinator.
    ///
    /// Persisted so a save made during an upload is still recognized after a
    /// restart, even when the sidecar completion time is newer than that save.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    backup_attempts: BTreeMap<String, FileRevision>,
    /// Recorded path prefixes from another machine mapped onto local folders.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    path_aliases: Vec<PathAliasState>,
    /// Folders scanned for projects.
    pub watched_folders: Vec<PathBuf>,
    /// Folders used as backup destinations — an external drive, or a NAS share.
    ///
    /// Kept apart from `watched_folders`: one is where projects are *read
    /// from*, the other where copies are *written to*, and conflating them
    /// would eventually have Auru back a project up into itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_providers: Vec<PathBuf>,
    /// Provider catalogue ids this machine has authenticated or explicitly
    /// approved for no-auth access.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connected_providers: Vec<String>,
    /// Default destination for a project's first backup. Once a project has a
    /// sidecar, that project's own `primary` takes precedence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_provider: Option<String>,
    /// Projects added individually, outside any watched folder.
    pub projects: Vec<PathBuf>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            origin: None,
            schema: STATE_SCHEMA,
            display_name: String::new(),
            onboarding_complete: false,
            appearance: "night".to_owned(),
            automatic_backups: true,
            verify_uploads: true,
            version_retention: "everything".to_owned(),
            sort_order: "attention".to_owned(),
            first_seen: BTreeMap::new(),
            backup_attempts: BTreeMap::new(),
            path_aliases: Vec::new(),
            watched_folders: Vec::new(),
            local_providers: Vec::new(),
            connected_providers: Vec::new(),
            primary_provider: None,
            projects: Vec::new(),
        }
    }
}

impl AppState {
    pub fn path_aliases(&self) -> &[PathAliasState] {
        &self.path_aliases
    }

    pub fn set_path_alias(&mut self, from: &str, to: &Path) {
        let from = from.trim().trim_end_matches(['/', '\\']).to_owned();
        if from.is_empty() {
            return;
        }
        let replacement = PathAliasState {
            from,
            to: to.into(),
        };
        match self.path_aliases.iter().position(|alias| {
            alias
                .from
                .trim_end_matches(['/', '\\'])
                .eq_ignore_ascii_case(&replacement.from)
        }) {
            Some(index) => self.path_aliases[index] = replacement,
            None => self.path_aliases.push(replacement),
        }
    }

    pub fn remove_path_alias(&mut self, from: &str) {
        let from = from.trim_end_matches(['/', '\\']);
        self.path_aliases.retain(|alias| {
            !alias
                .from
                .trim_end_matches(['/', '\\'])
                .eq_ignore_ascii_case(from)
        });
    }

    pub fn record_backup_attempt(&mut self, project_id: &str, modified_at: Option<SystemTime>) {
        let Some(revision) = modified_at.and_then(FileRevision::from_system_time) else {
            return;
        };
        self.backup_attempts.insert(project_id.to_owned(), revision);
    }

    pub fn backup_attempts(&self) -> impl Iterator<Item = (String, SystemTime)> + '_ {
        self.backup_attempts
            .iter()
            .filter_map(|(project_id, revision)| {
                Some((project_id.clone(), revision.to_system_time()?))
            })
    }

    /// Where the state file lives on this platform.
    ///
    /// `None` when the platform gives us nowhere to write, in which case the
    /// app runs perfectly well and simply forgets — better than refusing to
    /// start over a preferences file.
    pub fn path() -> Option<PathBuf> {
        Some(config_dir()?.join("state.json"))
    }

    /// Read the saved state, or start fresh.
    ///
    /// Any problem — missing, unreadable, malformed, written by a newer
    /// build — yields defaults. Losing preferences is a small harm; refusing
    /// to open someone's music because a settings file confused us is not.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Self {
        let mut state = Self::read_from(path);
        // Remember where it came from, whether or not it parsed: a state that
        // fell back to defaults still belongs to this file.
        state.origin = Some(path.to_path_buf());
        state
    }

    fn read_from(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(state) if state.schema <= STATE_SCHEMA => state,
            Ok(state) => {
                eprintln!(
                    "[auru-pm] {} was written by a newer version (schema {}); starting fresh",
                    path.display(),
                    state.schema
                );
                Self::default()
            }
            Err(error) => {
                eprintln!("[auru-pm] couldn't read {}: {error}", path.display());
                Self::default()
            }
        }
    }

    /// Write the state back where it came from.
    ///
    /// Best-effort: a failure is reported, never fatal. A state with no origin
    /// — one built in memory — writes nowhere at all.
    pub fn save(&self) {
        if let Err(error) = self.save_checked() {
            eprintln!("[auru-pm] {error}");
        }
    }

    /// Save preferences while allowing an invariant-sensitive caller to stop
    /// if the durable write fails.
    pub fn save_checked(&self) -> Result<(), String> {
        let Some(path) = self.origin.as_deref() else {
            return Ok(());
        };
        self.save_to(path)
            .map_err(|error| format!("couldn't save {}: {error}", path.display()))
    }

    /// Write via a temporary file and rename, so an interrupted save cannot
    /// leave behind a half-written file that the next launch discards.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_vec_pretty(self)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &body)?;
        match std::fs::rename(&tmp, path) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = std::fs::remove_file(&tmp);
                Err(error)
            }
        }
    }

    /// Add a watched folder, ignoring one already being watched.
    pub fn watch(&mut self, path: &Path) {
        if !self.watched_folders.iter().any(|known| known == path) {
            self.watched_folders.push(path.to_path_buf());
        }
    }

    /// Add an individually-added project.
    ///
    /// A project already covered by a watched folder is not recorded again —
    /// it would be found by the scan anyway, and listing it twice would make
    /// removing the folder leave a stray entry behind.
    pub fn add_project(&mut self, root: &Path) {
        if self.is_watched(root) {
            return;
        }
        if !self.projects.iter().any(|known| known == root) {
            self.projects.push(root.to_path_buf());
        }
    }

    /// Add a backup destination folder, ignoring one already recorded.
    pub fn add_local_provider(&mut self, path: &Path) {
        if !self.local_providers.iter().any(|known| known == path) {
            self.local_providers.push(path.to_path_buf());
        }
    }

    pub fn connect_provider(&mut self, provider_id: &str) {
        if !self
            .connected_providers
            .iter()
            .any(|known| known == provider_id)
        {
            self.connected_providers.push(provider_id.to_owned());
        }
        if self.primary_provider.is_none() {
            self.primary_provider = Some(provider_id.to_owned());
        }
    }

    pub fn is_provider_connected(&self, provider_id: &str) -> bool {
        self.connected_providers
            .iter()
            .any(|known| known == provider_id)
    }

    /// Record when a project was first seen, and return that time.
    ///
    /// Called for every project on every library load, so a project keeps the
    /// time it was *first* found rather than the time of the latest scan.
    ///
    /// Entries are never pruned. A project on an unplugged drive is missing
    /// from the scan but not gone, and forgetting when it was added — only to
    /// re-add it as brand new when the drive comes back — would be worse than
    /// the few bytes a stale entry costs.
    pub fn note_first_seen(&mut self, root: &str) -> i64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_secs() as i64)
            .unwrap_or_default();
        *self.first_seen.entry(root.to_owned()).or_insert(now)
    }

    /// Whether `root` sits inside a folder already being watched.
    pub fn is_watched(&self, root: &Path) -> bool {
        self.watched_folders
            .iter()
            .any(|folder| root.starts_with(folder))
    }
}

/// Auru's configuration directory for this platform.
fn config_dir() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        return std::env::var_os("APPDATA").map(|base| PathBuf::from(base).join("Auru").join("pm"));
    }

    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        return home.map(|home| {
            home.join("Library")
                .join("Application Support")
                .join("studio.auru.pm")
        });
    }

    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.map(|home| home.join(".config")))
        .map(|base| base.join("auru-pm"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_should_survive_a_round_trip() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("state.json");

        let mut state = AppState {
            display_name: "Jake".to_owned(),
            appearance: "day".to_owned(),
            automatic_backups: false,
            verify_uploads: false,
            version_retention: "last-fifty".to_owned(),
            ..AppState::default()
        };
        state.watch(Path::new("/music/Ableton Projects"));
        state.add_project(Path::new("/elsewhere/One Off Project"));
        state.connect_provider("studio-nas");
        let attempted_revision = UNIX_EPOCH + std::time::Duration::new(1_234, 567);
        state.record_backup_attempt("project:/music/Song.dawproject", Some(attempted_revision));
        state.save_to(&path).expect("save");

        let loaded = AppState::load_from(&path);
        assert_eq!(loaded.display_name, "Jake");
        assert_eq!(loaded.appearance, "day");
        assert!(!loaded.automatic_backups);
        assert!(!loaded.verify_uploads);
        assert_eq!(loaded.version_retention, "last-fifty");
        assert_eq!(loaded.watched_folders.len(), 1);
        assert_eq!(loaded.projects.len(), 1);
        assert!(loaded.is_provider_connected("studio-nas"));
        assert_eq!(loaded.connected_providers, vec!["studio-nas"]);
        assert_eq!(loaded.primary_provider.as_deref(), Some("studio-nas"));
        assert_eq!(
            loaded.backup_attempts().collect::<Vec<_>>(),
            vec![(
                "project:/music/Song.dawproject".to_owned(),
                attempted_revision
            )]
        );
    }

    #[test]
    fn path_aliases_should_persist_and_replace_the_same_recorded_prefix() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("state.json");
        let mut state = AppState::load_from(&path);

        state.set_path_alias(r"D:\Packs", Path::new("/mnt/old-packs"));
        state.set_path_alias(r"d:\packs\\", Path::new("/mnt/current-packs"));
        state.save();

        let loaded = AppState::load_from(&path);
        assert_eq!(
            loaded.path_aliases(),
            &[PathAliasState {
                from: r"d:\packs".to_owned(),
                to: PathBuf::from("/mnt/current-packs"),
            }]
        );
    }

    #[test]
    fn onboarding_completion_should_distinguish_new_and_existing_profiles() {
        assert!(!AppState::default().onboarding_complete);

        let temp = tempfile::tempdir().expect("tempdir");
        let legacy = temp.path().join("legacy.json");
        std::fs::write(&legacy, r#"{"schema":1,"display_name":"Existing user"}"#)
            .expect("legacy state");

        let loaded = AppState::load_from(&legacy);
        assert!(
            loaded.onboarding_complete,
            "an existing profile must not be forced through new setup steps"
        );
    }

    #[test]
    fn a_state_built_in_memory_should_never_write_to_the_real_config() {
        // `load_library` persists first-seen times, so a throwaway state that
        // could reach the user's own file would overwrite their settings when
        // the test suite ran. It did, once.
        let mut state = AppState {
            display_name: "should not be written".to_owned(),
            ..AppState::default()
        };
        state.note_first_seen("/somewhere");

        // No origin, so nothing to write to — and nothing written.
        state.save();

        let on_disk = AppState::path()
            .map(|path| AppState::load_from(&path).display_name)
            .unwrap_or_default();
        assert_ne!(on_disk, "should not be written");
    }

    #[test]
    fn a_state_read_from_a_file_should_save_back_to_that_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("state.json");
        AppState::default().save_to(&path).expect("seed");

        let mut state = AppState::load_from(&path);
        state.display_name = "Jake".to_owned();
        state.save();

        assert_eq!(AppState::load_from(&path).display_name, "Jake");
    }

    #[test]
    fn a_missing_file_should_start_fresh() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = AppState::load_from(&temp.path().join("nothing-here.json"));
        assert!(state.watched_folders.is_empty());
        assert_eq!(state.schema, STATE_SCHEMA);
    }

    #[test]
    fn a_corrupt_file_should_start_fresh_rather_than_fail() {
        // Refusing to open someone's music over a broken preferences file
        // would be a far worse outcome than losing their preferences.
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("state.json");
        std::fs::write(&path, "{ not json at all").expect("write");

        assert!(AppState::load_from(&path).watched_folders.is_empty());
    }

    #[test]
    fn a_file_from_a_newer_build_should_not_be_guessed_at() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("state.json");
        let state = AppState {
            schema: STATE_SCHEMA + 1,
            display_name: "From the future".to_owned(),
            ..AppState::default()
        };
        state.save_to(&path).expect("save");

        let loaded = AppState::load_from(&path);
        assert_eq!(
            loaded.display_name, "",
            "fields we may not understand are not adopted"
        );
    }

    #[test]
    fn a_file_missing_fields_should_fill_them_in() {
        // Anything written by an older build must keep working.
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("state.json");
        std::fs::write(&path, r#"{"display_name":"Jake"}"#).expect("write");

        let loaded = AppState::load_from(&path);
        assert_eq!(loaded.display_name, "Jake");
        assert!(loaded.automatic_backups, "defaults fill the rest");
        assert_eq!(loaded.appearance, "night");
    }

    #[test]
    fn watching_the_same_folder_twice_should_record_it_once() {
        let mut state = AppState::default();
        state.watch(Path::new("/music"));
        state.watch(Path::new("/music"));
        assert_eq!(state.watched_folders.len(), 1);
    }

    #[test]
    fn a_project_inside_a_watched_folder_should_not_be_listed_separately() {
        // The scan finds it anyway. Recording it twice would leave a stray
        // entry behind when the folder is unwatched.
        let mut state = AppState::default();
        state.watch(Path::new("/music/Ableton Projects"));
        state.add_project(Path::new("/music/Ableton Projects/Song Project"));

        assert!(state.projects.is_empty());
        assert!(state.is_watched(Path::new("/music/Ableton Projects/Song Project")));
    }

    #[test]
    fn a_project_outside_every_watched_folder_should_be_kept() {
        let mut state = AppState::default();
        state.watch(Path::new("/music/Ableton Projects"));
        state.add_project(Path::new("/elsewhere/Song Project"));
        assert_eq!(state.projects.len(), 1);
    }

    #[test]
    fn an_interrupted_save_should_not_leave_a_stray_file_behind() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("state.json");
        AppState::default().save_to(&path).expect("save");

        let strays: Vec<_> = std::fs::read_dir(temp.path())
            .expect("read dir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty(), "left behind {strays:?}");
    }
}

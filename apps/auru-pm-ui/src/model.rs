use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use auru_pm::{
    CommitId, CommitSummary, DiscoveredProject, PluginAvailability, PluginSearchPaths,
    ProjectFormat, ProjectInfo, ProjectLocation, ProjectMetadata, ProjectSnapshot, ResolvedPlugin,
    Sidecar, ableton, discovery, flstudio, plugin_registry, sidecar_path_for,
};

/// A folder Auru watches for projects.
///
/// Watching is not backing up. A folder is scanned so a person can see what is
/// in it and decide; nothing leaves the machine until they say so. On a paid
/// provider that distinction is the difference between a considered choice and
/// an unexpected bill, which is why the setup screen says
/// `NOTHING UPLOADS UNTIL YOU FINISH` and means it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchedFolder {
    pub path: PathBuf,
    /// Projects found inside, in the order they will be listed.
    pub projects: Vec<FoundProject>,
}

impl WatchedFolder {
    /// Scan `path` and record what is in it.
    pub fn scan(path: &Path) -> Self {
        let projects = discovery::scan_for_projects(path, &discovery::ScanOptions::default())
            .into_iter()
            .map(|found| FoundProject::from_discovered(path, &found))
            .collect();
        Self {
            path: path.to_path_buf(),
            projects,
        }
    }

    /// How the folder is shown in the watched list, eg `"~/Music/Ableton"`.
    pub fn display_path(&self) -> String {
        shorten_home(&self.path)
    }

    pub fn project_count(&self) -> usize {
        self.projects.len()
    }

    /// Bytes across every project found, as far as they could be measured.
    pub fn total_bytes(&self) -> u64 {
        self.projects.iter().map(|project| project.bytes).sum()
    }
}

/// A project a scan turned up.
///
/// Deliberately shallow: a scan of a real library found 653 projects, and
/// opening each one to read its tempo would mean gunzipping hundreds of
/// megabytes of XML to draw a list. The detail is read when a project is
/// actually opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundProject {
    pub name: String,
    pub root: PathBuf,
    /// Portable path beneath the folder that was scanned.
    pub location: Option<ProjectLocation>,
    /// The DAW project file name, shown under the project name.
    pub file: String,
    /// Size of the project folder on disk.
    pub bytes: u64,
}

impl FoundProject {
    fn from_discovered(library_root: &Path, found: &DiscoveredProject) -> Self {
        let project_file = found.project_file();
        let root = if found.owns_its_directory() {
            found.directory()
        } else {
            project_file
        };
        Self {
            name: found.name(),
            // What adding this project means: a folder for Ableton, a single
            // file for FL. Keying on the folder for FL would make two projects
            // saved side by side look like the same one.
            root: root.to_path_buf(),
            location: portable_project_location(library_root, root),
            file: project_file
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned(),
            // Measuring the containing folder would be wrong for a project
            // that does not own it — an `.flp` saved into Downloads would
            // report the size of everything else in there.
            bytes: if found.owns_its_directory() {
                folder_bytes(found.directory())
            } else {
                std::fs::metadata(project_file).map_or(0, |meta| meta.len())
            },
        }
    }

    /// eg `"1.2 GB"`. Blank when the size could not be measured.
    pub fn size_label(&self) -> String {
        format_bytes(self.bytes)
    }

    pub fn library_path(&self) -> &str {
        self.location
            .as_ref()
            .map(|location| location.relative_path.as_str())
            .unwrap_or(&self.name)
    }
}

/// Express one project beneath a watched root without leaking an absolute,
/// machine-specific path into its provider profile.
fn portable_project_location(library_root: &Path, project: &Path) -> Option<ProjectLocation> {
    let relative = project.strip_prefix(library_root).ok()?;
    let mut parts = Vec::new();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return None;
        };
        parts.push(component.to_str()?.to_owned());
    }
    (!parts.is_empty()).then(|| ProjectLocation {
        relative_path: parts.join("/"),
    })
}

/// Total size of a folder, best-effort.
///
/// Bounded rather than exhaustive: a scan lists hundreds of projects and a
/// rough number drawn instantly is worth more here than an exact one that
/// takes a visible pause.
fn folder_bytes(root: &Path) -> u64 {
    const MAX_ENTRIES: usize = 4_000;
    let mut total = 0;
    let mut seen = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_ENTRIES {
                return total;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                stack.push(entry.path());
            } else {
                total += metadata.len();
            }
        }
    }
    total
}

/// When a file was last written, or `None` if we cannot tell.
///
/// Unknown rather than "the beginning of time": a project on an unplugged
/// drive has no modification time, and treating that as 1970 would bury it at
/// the bottom of a sort as though it were ancient.
fn file_modified_at(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

struct LocalFileState {
    status: ProjectStatus,
    modified_at: Option<SystemTime>,
    backed_up_at: Option<SystemTime>,
    last_activity: String,
}

fn local_file_state(project_file: &Path) -> LocalFileState {
    let status = ProjectStatus::read_from_disk(project_file);
    let modified_at = file_modified_at(project_file);
    LocalFileState {
        status,
        modified_at,
        backed_up_at: match status {
            ProjectStatus::NeverBackedUp => None,
            _ => file_modified_at(&sidecar_path_for(project_file)),
        },
        last_activity: describe_modified(modified_at),
    }
}

#[derive(Default)]
struct ProviderLinks {
    remote: Option<RemoteProjectRef>,
    handles: BTreeMap<String, String>,
    metadata: ProjectMetadata,
    location: Option<ProjectLocation>,
}

fn provider_links_for(project_path: &Path) -> ProviderLinks {
    let Ok(sidecar) = Sidecar::load(&sidecar_path_for(project_path)) else {
        return ProviderLinks::default();
    };
    let remote = sidecar.primary.as_ref().and_then(|provider_id| {
        Some(RemoteProjectRef {
            provider_id: provider_id.clone(),
            handle: sidecar.provider_handles.get(provider_id)?.clone(),
            head: sidecar.local_head?,
        })
    });
    ProviderLinks {
        remote,
        handles: sidecar.provider_handles,
        metadata: sidecar.metadata,
        location: sidecar.location,
    }
}

/// How long ago a file was saved, in the words a person would use.
fn describe_modified(modified: Option<SystemTime>) -> String {
    let Some(modified) = modified else {
        return "on this computer".to_owned();
    };
    let Ok(elapsed) = modified.elapsed() else {
        // A file dated in the future is a clock problem, not a project one.
        return "saved just now".to_owned();
    };

    let minutes = elapsed.as_secs() / 60;
    let hours = minutes / 60;
    let days = hours / 24;
    match (days, hours, minutes) {
        (0, 0, 0) => "saved just now".to_owned(),
        (0, 0, 1) => "saved 1 minute ago".to_owned(),
        (0, 0, minutes) => format!("saved {minutes} minutes ago"),
        (0, 1, _) => "saved an hour ago".to_owned(),
        (0, hours, _) => format!("saved {hours} hours ago"),
        (1, _, _) => "saved yesterday".to_owned(),
        (days, _, _) if days < 30 => format!("saved {days} days ago"),
        (days, _, _) if days < 365 => format!("saved {} months ago", days / 30),
        (days, _, _) => format!("saved {} years ago", days / 365),
    }
}

fn describe_epoch(timestamp: i64) -> String {
    u64::try_from(timestamp)
        .ok()
        .and_then(|seconds| UNIX_EPOCH.checked_add(Duration::from_secs(seconds)))
        .map(|time| describe_modified(Some(time)))
        .unwrap_or_else(|| "date unknown".to_owned())
}

/// Human-readable byte count, in the units a musician thinks in.
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let bytes = bytes as f64;
    if bytes >= GB {
        format!("{:.1} GB", bytes / GB)
    } else if bytes >= MB {
        format!("{:.0} MB", bytes / MB)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes / KB)
    } else {
        format!("{bytes:.0} B")
    }
}

/// Replace the home directory with `~`, the way a path is written down.
fn shorten_home(path: &Path) -> String {
    let text = path.display().to_string();
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return text;
    };
    let home = home.display().to_string();
    match text.strip_prefix(&home) {
        Some(rest) => format!("~{rest}"),
        None => text,
    }
}

/// What reading a Live Set turned up, ready to fold into a [`Project`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoadedDetail {
    pub size: String,
    pub inventory: String,
    pub detail: Option<ProjectDetail>,
    pub missing_plugins: Vec<MissingPlugin>,
}

/// A kind of project that can be added from disk.
///
/// One variant per DAW rather than a single "open a file" action, because the
/// DAWs genuinely differ in what you point at: an Ableton project is a folder,
/// the others are single files. Asking which kind first lets the file dialog
/// ask for the right thing instead of making the user work it out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportKind {
    AbletonLiveSet,
    BitwigProject,
    FlStudio,
    Dawproject,
    AuruProject,
}

impl ImportKind {
    /// Every kind that can be added, in menu order.
    pub const ALL: [Self; 5] = [
        Self::AbletonLiveSet,
        Self::BitwigProject,
        Self::FlStudio,
        Self::Dawproject,
        Self::AuruProject,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::AbletonLiveSet => "Ableton Live project…",
            Self::BitwigProject => "Bitwig Studio project…",
            Self::FlStudio => "FL Studio project…",
            Self::Dawproject => "DAWproject file…",
            Self::AuruProject => "Auru project…",
        }
    }

    /// What the file dialog should say it is looking for.
    pub const fn prompt(self) -> &'static str {
        match self {
            Self::AbletonLiveSet => "Choose an Ableton project folder or .als",
            Self::BitwigProject => "Choose a .bwproject",
            Self::FlStudio => "Choose an .flp",
            Self::Dawproject => "Choose a .dawproject file",
            Self::AuruProject => "Choose an .auru file",
        }
    }

    /// Whether picking a folder makes sense for this kind.
    ///
    /// Ableton's unit of work is the project folder, so that is offered first
    /// — but a loose `.als` is a legitimate thing to have, and the detector
    /// handles either.
    pub const fn accepts_directories(self) -> bool {
        matches!(self, Self::AbletonLiveSet)
    }

    pub const fn format(self) -> ProjectFormat {
        match self {
            Self::AbletonLiveSet => ProjectFormat::AbletonLiveSet,
            Self::BitwigProject => ProjectFormat::BitwigProject,
            Self::FlStudio => ProjectFormat::FlStudio,
            Self::Dawproject => ProjectFormat::Dawproject,
            Self::AuruProject => ProjectFormat::Auru,
        }
    }
}

/// Add a project from disk.
///
/// Reads the project through the same path everything else uses, so what lands
/// in the library is the real thing: its tempo and key come out of the file,
/// and its plugin list is resolved against this machine.
///
/// The error is written for the person who picked the file, not for a log.
pub fn import_project(kind: ImportKind, path: &Path) -> Result<Project, String> {
    // For Ableton, the folder is the project; a `.als` inside one resolves to
    // the same place. Anything else is the file itself.
    let bundle = matches!(kind, ImportKind::AbletonLiveSet)
        .then(|| ableton::AbletonBundle::detect(path).ok().flatten())
        .flatten();
    let project_file = bundle.as_ref().map_or_else(
        || path.to_path_buf(),
        |bundle| bundle.live_set().to_path_buf(),
    );

    if project_file.is_dir() {
        return Err(format!(
            "{} doesn't contain a project Auru can read.",
            display_name(path)
        ));
    }

    let snapshot = ProjectSnapshot::load(&project_file)
        .map_err(|error| format!("Couldn't read {}. {error}", display_name(&project_file)))?;

    if snapshot.format() != kind.format() {
        return Err(format!(
            "{} is a {} project, not {}.",
            display_name(&project_file),
            snapshot.format(),
            kind.format()
        ));
    }

    let detail = ProjectInfo::from_snapshot_bytes(snapshot.as_bytes())
        .as_ref()
        .and_then(ProjectDetail::from_project_info);

    let missing_plugins = missing_plugins_for(&snapshot);

    let name = project_file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Untitled")
        .to_owned();
    let file_name = display_name(&project_file);
    let root = bundle
        .as_ref()
        .map_or(project_file.as_path(), |bundle| bundle.root());

    let links = provider_links_for(&project_file);
    Ok(Project {
        // Path-derived so re-adding the same project replaces rather than
        // duplicates it.
        id: format!("imported:{}", root.display()),
        name,
        file_name,
        local_path: root.display().to_string(),
        size: describe_size(&snapshot, detail.as_ref()),
        format: snapshot.format(),
        // It is on this computer and has never been backed up — which is
        // precisely the state worth drawing attention to.
        status: ProjectStatus::OutOfSync(SyncDirection::LocalAhead),
        last_activity: "added just now".to_owned(),
        safe_version: "not backed up yet".to_owned(),
        local_inventory: detail
            .as_ref()
            .map_or_else(|| "ready to back up".to_owned(), ProjectDetail::files_line),
        versions: Vec::new(),
        sync_progress: 0.0,
        modified_at: file_modified_at(&project_file),
        // Just imported, so no backup exists yet by definition.
        backed_up_at: None,
        // Stamped by `load_library` once this project is part of the library.
        added_at: None,
        live_set: Some(project_file),
        remote: links.remote,
        provider_handles: links.handles,
        metadata: links.metadata,
        location: links.location,
        detail,
        missing_plugins,
    })
}

/// Plugins this computer does not have, for a project of any format.
///
/// Each DAW records plugin identity differently — Ableton in its XML, FL
/// inside opaque plugin state — so the reading is per format, but what comes
/// back is the same question answered: what would fail to load here.
fn missing_plugins_for(snapshot: &ProjectSnapshot) -> Vec<MissingPlugin> {
    let plugins = match snapshot.format() {
        ProjectFormat::FlStudio => snapshot
            .restore_bytes()
            .ok()
            .and_then(|bytes| flstudio::read_plugins(&bytes).ok()),
        ProjectFormat::Dawproject => auru_pm::dawproject::read_plugins(snapshot).ok(),
        ProjectFormat::AbletonLiveSet => ableton::read_plugins(snapshot).ok(),
        ProjectFormat::Auru | ProjectFormat::BitwigProject => None,
    };

    plugins
        .map(|plugins| {
            plugin_registry::resolve(
                &plugins,
                plugin_registry::bundled(),
                &PluginSearchPaths::detect(),
            )
            .iter()
            .filter_map(MissingPlugin::from_resolved)
            .collect()
        })
        .unwrap_or_default()
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("this project")
        .to_owned()
}

/// A rough size for the library row.
///
/// The snapshot is the project's own weight; the files it depends on are
/// counted separately and are usually far larger, so this says what it is
/// measuring rather than implying a total it has not computed.
fn describe_size(snapshot: &ProjectSnapshot, detail: Option<&ProjectDetail>) -> String {
    let mib = snapshot.as_bytes().len() as f64 / (1024.0 * 1024.0);
    match detail {
        Some(detail) if detail.files_total > 0 => {
            format!("{mib:.1} MB project · {}", detail.files_line())
        }
        _ => format!("{mib:.1} MB project"),
    }
}

/// What a project *is*, as shown on its detail page.
///
/// Built from the [`ProjectInfo`] every commit carries, so the detail on
/// screen is read out of the project rather than described alongside it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProjectDetail {
    pub tempo: Option<f64>,
    pub time_signature: Option<(u32, u32)>,
    /// Human-readable key, eg `"C Phrygian"`.
    pub key: Option<String>,
    pub in_key: bool,
    pub tracks_total: usize,
    pub tracks_midi: usize,
    pub tracks_audio: usize,
    pub tracks_return: usize,
    pub clip_count: usize,
    pub bars: Option<f64>,
    pub live_version: Option<String>,
    /// Files the project depends on, and how many must travel with it.
    pub files_total: usize,
    pub files_gathered: usize,
}

impl ProjectDetail {
    /// Read the detail out of a commit's project summary.
    ///
    /// `None` for a format whose detail Auru does not read, so the detail page
    /// falls back to what it showed before rather than displaying zeroes.
    pub fn from_project_info(info: &ProjectInfo) -> Option<Self> {
        if let Some(fl) = info.flstudio.as_ref() {
            return Some(Self::from_flstudio(fl));
        }
        if let Some(dawproject) = info.dawproject.as_ref() {
            return Some(Self::from_dawproject(dawproject));
        }
        let ableton = info.ableton.as_ref()?;
        Some(Self {
            tempo: ableton.tempo,
            time_signature: ableton
                .time_signature
                .map(|sig| (sig.numerator, sig.denominator)),
            key: ableton.key.as_ref().map(|key| key.label()),
            in_key: ableton.key.as_ref().is_some_and(|key| key.in_key),
            tracks_total: ableton.tracks.total(),
            tracks_midi: ableton.tracks.midi,
            tracks_audio: ableton.tracks.audio,
            tracks_return: ableton.tracks.retn,
            clip_count: ableton.clip_count,
            bars: ableton.arrangement_bars(),
            live_version: ableton.live_version.clone(),
            files_total: ableton.assets.total(),
            files_gathered: ableton.assets.vendorable(),
        })
    }

    /// Read the detail out of an FL Studio summary.
    ///
    /// FL describes a project in its own terms, so several fields have no
    /// counterpart and stay empty rather than being filled with a plausible
    /// zero: FL has no project-wide key, and its channel rack is not a track
    /// list. The channel count is reported as the track total because that is
    /// the number a person recognises the project by.
    fn from_flstudio(fl: &flstudio::FlStudioMetadata) -> Self {
        Self {
            tempo: fl.tempo,
            time_signature: fl.time_signature,
            key: None,
            in_key: false,
            tracks_total: usize::from(fl.channels),
            tracks_midi: 0,
            tracks_audio: 0,
            tracks_return: 0,
            clip_count: fl.pattern_names.len(),
            bars: None,
            live_version: fl.version.clone(),
            files_total: fl.assets.total,
            files_gathered: fl.assets.vendored(),
        }
    }

    fn from_dawproject(metadata: &auru_pm::DawprojectMetadata) -> Self {
        let bars = metadata.time_signature.and_then(|signature| {
            let beats_per_bar = signature.beats_per_bar();
            (beats_per_bar > 0.0).then(|| metadata.arrangement_end_beats / beats_per_bar)
        });
        Self {
            tempo: metadata.tempo,
            time_signature: metadata
                .time_signature
                .map(|signature| (signature.numerator, signature.denominator)),
            // DAWproject 1.0 has no project-wide key field.
            key: None,
            in_key: false,
            tracks_total: metadata.tracks.total(),
            tracks_midi: metadata.tracks.notes + metadata.tracks.hybrid,
            tracks_audio: metadata.tracks.audio + metadata.tracks.hybrid,
            tracks_return: metadata.tracks.effect,
            clip_count: metadata.clip_count,
            bars,
            live_version: metadata.application_label(),
            files_total: metadata.assets.total(),
            files_gathered: metadata.assets.embedded,
        }
    }

    /// `"175 BPM · 4/4"`, omitting whatever the project did not declare.
    pub fn tempo_line(&self) -> String {
        let mut parts = Vec::new();
        if let Some(tempo) = self.tempo {
            parts.push(if tempo.fract().abs() < f64::EPSILON {
                format!("{tempo:.0} BPM")
            } else {
                format!("{tempo:.2} BPM")
            });
        }
        if let Some((numerator, denominator)) = self.time_signature {
            parts.push(format!("{numerator}/{denominator}"));
        }
        join_or_dash(parts)
    }

    pub fn key_line(&self) -> String {
        match &self.key {
            Some(key) if self.in_key => format!("{key} · in key"),
            Some(key) => key.clone(),
            None => "—".to_owned(),
        }
    }

    /// `"16 tracks · 9 MIDI · 4 audio · 2 returns"`.
    pub fn tracks_line(&self) -> String {
        if self.tracks_total == 0 {
            return "—".to_owned();
        }
        let mut parts = vec![plural(self.tracks_total, "track")];
        if self.tracks_midi > 0 {
            parts.push(format!("{} MIDI", self.tracks_midi));
        }
        if self.tracks_audio > 0 {
            parts.push(format!("{} audio", self.tracks_audio));
        }
        if self.tracks_return > 0 {
            parts.push(format!("{} returns", self.tracks_return));
        }
        parts.join(" · ")
    }

    /// `"88 bars · 53 clips"`.
    pub fn length_line(&self) -> String {
        let mut parts = Vec::new();
        if let Some(bars) = self.bars.filter(|bars| *bars >= 1.0) {
            parts.push(plural(bars.round() as usize, "bar"));
        }
        if self.clip_count > 0 {
            parts.push(plural(self.clip_count, "clip"));
        }
        join_or_dash(parts)
    }

    pub fn made_with(&self) -> String {
        self.live_version
            .clone()
            .unwrap_or_else(|| "Ableton Live".to_owned())
    }

    /// `"27 files · 5 travel with the project"`.
    pub fn files_line(&self) -> String {
        if self.files_total == 0 {
            return "—".to_owned();
        }
        let files = plural(self.files_total, "file");
        if self.files_gathered == 0 {
            files
        } else {
            format!("{files} · {} gathered in", self.files_gathered)
        }
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("{count} {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn join_or_dash(parts: Vec<String>) -> String {
    if parts.is_empty() {
        "—".to_owned()
    } else {
        parts.join(" · ")
    }
}

/// A plugin the project needs that this computer does not have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingPlugin {
    pub name: String,
    pub vendor: String,
    /// `"VST2"`, `"VST3"`, `"AU"`.
    pub format: String,
    pub instances: usize,
    /// Where the maker distributes it, when the registry knows.
    pub link: Option<String>,
}

impl MissingPlugin {
    /// `None` for anything already available, so only what needs the user's
    /// attention reaches the screen.
    pub fn from_resolved(plugin: &ResolvedPlugin) -> Option<Self> {
        if plugin.availability != PluginAvailability::NotOnThisComputer {
            return None;
        }
        Some(Self {
            name: plugin.name.clone(),
            vendor: plugin.vendor.clone(),
            format: plugin.format.label().to_owned(),
            instances: plugin.instances,
            link: plugin.link().map(str::to_owned),
        })
    }

    /// `"Xfer Records · VST3 · used 7 times"`.
    pub fn detail_line(&self) -> String {
        let mut parts = Vec::new();
        if !self.vendor.is_empty() {
            parts.push(self.vendor.clone());
        }
        parts.push(self.format.clone());
        parts.push(if self.instances == 1 {
            "used once".to_owned()
        } else {
            format!("used {} times", self.instances)
        });
        parts.join(" · ")
    }
}

/// The line shown beneath a missing-plugin list.
///
/// It is the whole reassurance the feature exists to give, and it is true: a
/// project stores its plugin settings itself, so nothing is lost by not having
/// the plugin today.
pub const PLUGIN_SETTINGS_REASSURANCE: &str = "Your settings for these are saved inside the project. They come back exactly as you \
     left them once the plugin is installed and authorized on this computer.";

/// Which side has work the other side has not seen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncDirection {
    LocalAhead,
    UpstreamAhead,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectStatus {
    /// On this computer and nowhere else — no backup has ever been made.
    ///
    /// The starting state for every project Auru finds, and the one worth
    /// drawing attention to: the work exists in exactly one place.
    NeverBackedUp,
    NotDownloaded,
    Downloaded,
    Syncing,
    OutOfSync(SyncDirection),
    Conflicted,
}

impl ProjectStatus {
    /// Work out a project's state from what is on disk.
    ///
    /// Deliberately cheap. A real library runs to hundreds of projects, and
    /// reading each one's snapshot to compare it against its last commit would
    /// cost a minute of gunzipping before the window could be drawn. Two
    /// signals are enough to be truthful without opening anything:
    ///
    /// - whether the sidecar records a commit at all, and
    /// - whether the project has been saved since that sidecar was written.
    ///
    /// The second is a comparison of modification times, so it answers "you
    /// have saved since your last backup" rather than "the contents differ".
    /// Those come apart only when a save changed nothing, which errs toward
    /// offering a backup that turns out to be a no-op — the harmless direction.
    pub fn read_from_disk(live_set: &Path) -> Self {
        let sidecar_path = sidecar_path_for(live_set);
        let Ok(sidecar) = Sidecar::load(&sidecar_path) else {
            return Self::NeverBackedUp;
        };
        if sidecar.local_head.is_none() {
            return Self::NeverBackedUp;
        }

        let modified = |path: &Path| {
            std::fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .ok()
        };
        match (modified(live_set), modified(&sidecar_path)) {
            (Some(project), Some(backup)) if project > backup => {
                Self::OutOfSync(SyncDirection::LocalAhead)
            }
            // Without both timestamps there is nothing to compare, and the
            // last thing we know for certain is that a backup was made.
            _ => Self::Downloaded,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::NeverBackedUp => "Never backed up",
            Self::NotDownloaded => "Ready to restore",
            Self::Downloaded => "Downloaded",
            Self::Syncing => "Syncing",
            Self::OutOfSync(SyncDirection::LocalAhead) => "Out of sync · local ahead",
            Self::OutOfSync(SyncDirection::UpstreamAhead) => "Out of sync · upstream ahead",
            Self::Conflicted => "Conflict",
        }
    }

    pub const fn action(self) -> ProjectAction {
        match self {
            Self::NeverBackedUp => ProjectAction::Push,
            Self::NotDownloaded => ProjectAction::Restore,
            Self::Downloaded => ProjectAction::Open,
            Self::Syncing => ProjectAction::None,
            Self::OutOfSync(SyncDirection::LocalAhead) => ProjectAction::Push,
            Self::OutOfSync(SyncDirection::UpstreamAhead) => ProjectAction::Pull,
            Self::Conflicted => ProjectAction::ReviewConflicts,
        }
    }

    pub const fn needs_attention(self) -> bool {
        matches!(
            self,
            Self::NeverBackedUp | Self::OutOfSync(_) | Self::Conflicted
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectAction {
    Restore,
    Open,
    Push,
    Pull,
    ReviewConflicts,
    None,
}

impl ProjectAction {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Restore => "↓  RESTORE PROJECT",
            Self::Open => "OPEN PROJECT",
            Self::Push => "↑  BACK UP CHANGES",
            Self::Pull => "↓  DOWNLOAD LATEST",
            Self::ReviewConflicts => "REVIEW CONFLICT",
            Self::None => "SYNCING…",
        }
    }

    pub const fn starts_transfer(self) -> bool {
        matches!(self, Self::Restore | Self::Push | Self::Pull)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectVersion {
    pub id: CommitId,
    pub version: String,
    pub summary: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteProjectRef {
    pub provider_id: String,
    pub handle: String,
    pub head: CommitId,
}

#[derive(Clone, Debug)]
pub struct RemoteProjectSeed {
    pub provider_id: String,
    pub provider_name: String,
    pub handle: String,
    pub head: CommitId,
    pub name: String,
    pub file_name: String,
    pub format: ProjectFormat,
    pub metadata: ProjectMetadata,
    pub location: Option<ProjectLocation>,
    pub updated_at: i64,
    pub info: Option<ProjectInfo>,
}

/// BPM constraints used by the library filter bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BpmFilter {
    Range { min: u16, max: u16 },
    Exact(u16),
}

impl BpmFilter {
    pub fn matches(self, tempo: Option<f64>) -> bool {
        let Some(tempo) = tempo.filter(|tempo| tempo.is_finite()) else {
            return false;
        };
        match self {
            Self::Range { min, max } => tempo >= f64::from(min) && tempo <= f64::from(max),
            Self::Exact(bpm) => tempo.round() == f64::from(bpm),
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::Range { min, max } => format!("{min}–{max}"),
            Self::Exact(bpm) => bpm.to_string(),
        }
    }
}

/// Unique metadata values available to the library filter comboboxes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LibraryFilterOptions {
    pub genres: Vec<String>,
    pub tags: Vec<String>,
}

pub fn library_filter_options(projects: &[Project]) -> LibraryFilterOptions {
    let mut genres = BTreeMap::new();
    let mut tags = BTreeMap::new();
    for project in projects {
        for genre in project.metadata.genres() {
            genres
                .entry(genre.to_lowercase())
                .or_insert_with(|| genre.to_owned());
        }
        for tag in &project.metadata.tags {
            let tag = tag.trim();
            if !tag.is_empty() {
                tags.entry(tag.to_lowercase())
                    .or_insert_with(|| tag.to_owned());
            }
        }
    }
    LibraryFilterOptions {
        genres: genres.into_values().collect(),
        tags: tags.into_values().collect(),
    }
}

#[derive(Debug)]
pub struct Project {
    /// Derived from the project's location so adding the same project twice
    /// replaces it rather than duplicating it.
    pub id: String,
    pub name: String,
    pub file_name: String,
    pub local_path: String,
    pub size: String,
    pub format: ProjectFormat,
    pub status: ProjectStatus,
    pub last_activity: String,
    pub safe_version: String,
    pub local_inventory: String,
    pub versions: Vec<ProjectVersion>,
    /// What the project is, read from its latest commit's summary. `None`
    /// until that summary has been fetched, or for a format Auru does not
    /// read detail from.
    /// Progress of a transfer in flight, 0.0–1.0.
    pub sync_progress: f32,
    /// When the Live Set was last saved. `None` when it cannot be read.
    pub modified_at: Option<SystemTime>,
    /// When this project was last backed up.
    ///
    /// Taken from the sidecar's own modification time, which is written each
    /// time a backup completes — so it answers "when did the safe copy last
    /// change", which is what someone sorting by it wants to know. `None`
    /// until a first backup exists.
    pub backed_up_at: Option<SystemTime>,
    /// Unix seconds when Auru first saw this project. See
    /// [`crate::state::AppState::note_first_seen`].
    pub added_at: Option<i64>,
    /// The `.als` on disk, for projects read from a real folder.
    pub live_set: Option<PathBuf>,
    /// Provider identity for both linked local projects and provider-only
    /// projects discovered through an account catalogue.
    pub remote: Option<RemoteProjectRef>,
    /// Every provider handle recorded for this project, cached from its
    /// sidecar so catalogue reconciliation never performs filesystem I/O.
    pub provider_handles: BTreeMap<String, String>,
    /// User-authored labels synced through the provider project profile.
    pub metadata: ProjectMetadata,
    /// Portable project placement beneath its watched library root.
    pub location: Option<ProjectLocation>,
    pub detail: Option<ProjectDetail>,
    /// Plugins this computer does not have. Empty is the good case.
    pub missing_plugins: Vec<MissingPlugin>,
}

impl Project {
    /// Read a project from a folder on disk, without opening its Live Set.
    ///
    /// Cheap on purpose: this runs once per project when the library loads,
    /// and a real library is hundreds of them. Tempo, key and plugins need the
    /// snapshot parsed, which costs a gunzip of several megabytes apiece — so
    /// that waits until a project is actually selected. See
    /// [`Self::detail_for`].
    ///
    /// `None` when the folder turns out not to hold a project after all.
    pub fn read_from_disk(root: &Path) -> Option<Self> {
        let found = DiscoveredProject::detect(root).ok().flatten()?;
        let live_set = found.project_file().to_path_buf();
        let local = local_file_state(&live_set);
        let links = provider_links_for(&live_set);

        Some(Self {
            // Keyed on the project file, not the folder: a folder can hold
            // several `.flp`s, and keying on it would make the second silently
            // replace the first.
            id: format!("project:{}", live_set.display()),
            name: live_set
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("Untitled")
                .to_owned(),
            file_name: live_set
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned(),
            local_path: found.directory().display().to_string(),
            size: String::new(),
            format: found.format(),
            status: local.status,
            last_activity: local.last_activity,
            safe_version: match local.status {
                ProjectStatus::NeverBackedUp => "not backed up yet".to_owned(),
                _ => "backed up".to_owned(),
            },
            local_inventory: String::new(),
            versions: Vec::new(),
            sync_progress: 0.0,
            modified_at: local.modified_at,
            backed_up_at: local.backed_up_at,
            added_at: None,
            detail: None,
            missing_plugins: Vec::new(),
            live_set: Some(live_set),
            remote: links.remote,
            provider_handles: links.handles,
            metadata: links.metadata,
            location: links.location,
        })
    }

    /// Refresh the cheap filesystem facts used by status and automatic backup.
    ///
    /// An upstream conflict or an active transfer carries more information
    /// than modification times, so polling never overwrites those states.
    pub fn refresh_local_file_state(&mut self) {
        let Some(project_file) = self.live_set.as_deref() else {
            return;
        };
        let local = local_file_state(project_file);
        self.modified_at = local.modified_at;
        self.last_activity = local.last_activity;
        if matches!(
            self.status,
            ProjectStatus::NotDownloaded
                | ProjectStatus::Syncing
                | ProjectStatus::OutOfSync(SyncDirection::UpstreamAhead)
                | ProjectStatus::Conflicted
        ) {
            return;
        }

        self.status = local.status;
        self.backed_up_at = local.backed_up_at;
    }

    pub fn from_remote(seed: RemoteProjectSeed) -> Self {
        let backed_up_at = u64::try_from(seed.updated_at)
            .ok()
            .and_then(|seconds| UNIX_EPOCH.checked_add(Duration::from_secs(seconds)));
        let detail = seed
            .info
            .as_ref()
            .and_then(ProjectDetail::from_project_info);
        let provider_handles = BTreeMap::from([(seed.provider_id.clone(), seed.handle.clone())]);
        Self {
            id: format!("remote:{}:{}", seed.provider_id, seed.handle),
            name: seed.name,
            file_name: seed.file_name,
            local_path: format!("Stored on {}", seed.provider_name),
            size: String::new(),
            format: seed.format,
            status: ProjectStatus::NotDownloaded,
            last_activity: format!("backed up · {}", describe_epoch(seed.updated_at)),
            safe_version: describe_epoch(seed.updated_at),
            local_inventory: detail
                .as_ref()
                .map(ProjectDetail::files_line)
                .unwrap_or_default(),
            versions: Vec::new(),
            sync_progress: 0.0,
            modified_at: None,
            backed_up_at,
            added_at: Some(seed.updated_at),
            live_set: None,
            remote: Some(RemoteProjectRef {
                provider_id: seed.provider_id,
                handle: seed.handle,
                head: seed.head,
            }),
            provider_handles,
            metadata: seed.metadata,
            location: seed.location,
            detail,
            missing_plugins: Vec::new(),
        }
    }

    /// Read a Live Set for everything the detail page shows.
    ///
    /// The expensive half of [`Self::read_from_disk`] — several megabytes of
    /// gunzip and parse — so it is a free function the caller can run off the
    /// UI thread and hand back via [`Self::apply_detail`].
    pub fn detail_for(live_set: &Path) -> LoadedDetail {
        let Ok(snapshot) = ProjectSnapshot::load(live_set) else {
            return LoadedDetail::default();
        };
        let detail = ProjectInfo::from_snapshot_bytes(snapshot.as_bytes())
            .as_ref()
            .and_then(ProjectDetail::from_project_info);
        let missing_plugins = missing_plugins_for(&snapshot);

        LoadedDetail {
            size: describe_size(&snapshot, detail.as_ref()),
            inventory: detail
                .as_ref()
                .map(ProjectDetail::files_line)
                .unwrap_or_default(),
            detail,
            missing_plugins,
        }
    }

    /// Whether this project matches what someone typed in the search box.
    ///
    /// Matches the project's name, file name, genre, and tags
    /// case-insensitively. An empty query matches everything, so clearing the
    /// box restores the list.
    pub fn matches_search(&self, query: &str) -> bool {
        let query = query.trim();
        if query.is_empty() {
            return true;
        }
        let query = query.to_lowercase();
        self.name.to_lowercase().contains(&query)
            || self.file_name.to_lowercase().contains(&query)
            || self
                .metadata
                .genre
                .as_ref()
                .is_some_and(|genre| genre.to_lowercase().contains(&query))
            || self
                .metadata
                .tags
                .iter()
                .any(|tag| tag.to_lowercase().contains(&query))
    }

    /// Match the search box and every active filter group.
    ///
    /// Multiple values within Genre or Tags are alternatives; the three
    /// groups combine with AND, matching the behavior of music-library filter
    /// bars where selecting another value broadens one facet.
    pub fn matches_library_filters(
        &self,
        query: &str,
        genres: &[String],
        tags: &[String],
        bpm: Option<BpmFilter>,
    ) -> bool {
        self.matches_search(query)
            && (genres.is_empty()
                || self.metadata.genres().any(|genre| {
                    genres
                        .iter()
                        .any(|selected| genre.eq_ignore_ascii_case(selected))
                }))
            && (tags.is_empty()
                || self.metadata.tags.iter().any(|tag| {
                    tags.iter()
                        .any(|selected| tag.eq_ignore_ascii_case(selected))
                }))
            && bpm.is_none_or(|filter| {
                filter.matches(self.detail.as_ref().and_then(|detail| detail.tempo))
            })
    }

    /// Fold in what [`Self::detail_for`] read.
    pub fn apply_detail(&mut self, loaded: LoadedDetail) {
        self.size = loaded.size;
        self.local_inventory = loaded.inventory;
        self.detail = loaded.detail;
        self.missing_plugins = loaded.missing_plugins;
    }

    pub const fn format_label(&self) -> &'static str {
        match self.format {
            ProjectFormat::Dawproject => "DAWPROJECT",
            ProjectFormat::AbletonLiveSet => "ABLETON LIVE SET",
            ProjectFormat::BitwigProject => "BITWIG STUDIO PROJECT",
            ProjectFormat::FlStudio => "FL STUDIO PROJECT",
            ProjectFormat::Auru => "AURU PROJECT",
        }
    }

    pub const fn open_label(&self) -> &'static str {
        match self.format {
            ProjectFormat::AbletonLiveSet => "OPEN IN ABLETON LIVE  ⌘↵",
            ProjectFormat::BitwigProject => "OPEN IN BITWIG STUDIO  ⌘↵",
            ProjectFormat::FlStudio => "OPEN IN FL STUDIO  ⌘↵",
            ProjectFormat::Dawproject => "OPEN IN YOUR DAW  ⌘↵",
            ProjectFormat::Auru => "OPEN IN AURU STUDIO  ⌘↵",
        }
    }

    pub fn list_status(&self) -> String {
        match self.status {
            ProjectStatus::NeverBackedUp => "only on this computer".to_owned(),
            ProjectStatus::NotDownloaded => "stored with your provider".to_owned(),
            ProjectStatus::Downloaded => self.last_activity.clone(),
            ProjectStatus::Syncing => "backing up…".to_owned(),
            ProjectStatus::OutOfSync(SyncDirection::LocalAhead) => {
                format!("changes waiting · {}", self.last_activity)
            }
            ProjectStatus::OutOfSync(SyncDirection::UpstreamAhead) => {
                "a newer provider version is available".to_owned()
            }
            ProjectStatus::Conflicted => "conflict · needs you".to_owned(),
        }
    }

    pub const fn status_headline(&self) -> &'static str {
        match self.status {
            ProjectStatus::NeverBackedUp => "This project exists only on this computer.",
            ProjectStatus::NotDownloaded => {
                "A safe copy lives with your provider — it isn't on this computer."
            }
            ProjectStatus::Downloaded => "Backed up and verified. Safe to unplug.",
            ProjectStatus::Syncing => "Copying and verifying this project now.",
            ProjectStatus::OutOfSync(SyncDirection::LocalAhead) => {
                "Changes on this computer aren't backed up."
            }
            ProjectStatus::OutOfSync(SyncDirection::UpstreamAhead) => {
                "A newer version was saved from another computer."
            }
            ProjectStatus::Conflicted => "This computer and the provider have different edits.",
        }
    }

    pub fn status_explanation(&self) -> String {
        match self.status {
            ProjectStatus::NeverBackedUp => {
                "If this computer is lost or stolen, so is this work.".to_owned()
            }
            ProjectStatus::NotDownloaded => {
                if self.size.is_empty() {
                    "Restore it beneath a library root on this computer.".to_owned()
                } else {
                    format!(
                        "Restore it beneath a library root on this computer ({}).",
                        self.size
                    )
                }
            }
            ProjectStatus::Downloaded => "Every file was verified on Auru Cloud.".to_owned(),
            ProjectStatus::Syncing => {
                "You can keep working. Auru PM will verify every file when it finishes.".to_owned()
            }
            ProjectStatus::OutOfSync(SyncDirection::LocalAhead) => {
                "If this laptop is lost now, these edits go with it.".to_owned()
            }
            ProjectStatus::OutOfSync(SyncDirection::UpstreamAhead) => {
                "This computer has yesterday's copy — download before you edit.".to_owned()
            }
            ProjectStatus::Conflicted => {
                "Nothing is lost. Compare both versions and choose what to keep.".to_owned()
            }
        }
    }

    pub fn begin_transfer(&mut self) -> bool {
        if !self.status.action().starts_transfer() {
            return false;
        }

        self.status = ProjectStatus::Syncing;
        self.sync_progress = 0.0;
        self.last_activity = "started just now".to_owned();
        true
    }

    pub fn finish_transfer(&mut self, history: Vec<CommitSummary>) {
        if self.status == ProjectStatus::Syncing {
            self.status = ProjectStatus::Downloaded;
            self.sync_progress = 1.0;
            self.last_activity = "backed up · just now".to_owned();
            self.backed_up_at = Some(SystemTime::now());
            self.apply_history(history);
        }
    }

    pub fn fail_transfer(&mut self) {
        if self.status != ProjectStatus::Syncing {
            return;
        }
        self.status = self
            .live_set
            .as_deref()
            .map(ProjectStatus::read_from_disk)
            .unwrap_or(ProjectStatus::OutOfSync(SyncDirection::LocalAhead));
        self.sync_progress = 0.0;
    }

    pub fn apply_history(&mut self, history: Vec<CommitSummary>) {
        let remote_head = history.first().map(|commit| commit.id);
        self.reconcile_remote_head(remote_head);
        if let Some(head) = remote_head
            && let Some(remote) = &mut self.remote
        {
            remote.head = head;
        }
        let total = history.len();
        self.versions = history
            .into_iter()
            .enumerate()
            .map(|(index, commit)| ProjectVersion {
                id: commit.id,
                version: format!("v{}", total.saturating_sub(index)),
                summary: if commit.message.trim().is_empty() {
                    "Saved version".to_owned()
                } else {
                    commit.message
                },
                created_at: describe_epoch(commit.timestamp),
            })
            .collect();
        if let Some(latest) = self.versions.first() {
            self.safe_version = latest.created_at.clone();
        }
    }

    fn reconcile_remote_head(&mut self, remote_head: Option<CommitId>) {
        let Some(project_path) = self.live_set.as_deref() else {
            return;
        };
        let Ok(sidecar) = Sidecar::load(&sidecar_path_for(project_path)) else {
            return;
        };
        if remote_head.is_none() || remote_head == sidecar.local_head {
            return;
        }
        self.status = if ProjectStatus::read_from_disk(project_path)
            == ProjectStatus::OutOfSync(SyncDirection::LocalAhead)
        {
            ProjectStatus::Conflicted
        } else {
            ProjectStatus::OutOfSync(SyncDirection::UpstreamAhead)
        };
    }

    /// How full the row's backup bar should be, 0.0–1.0.
    ///
    /// The bar answers "how much of this is safely off this computer". A
    /// transfer fills as it copies; a project that is backed up reads full; one
    /// that exists only in the cloud reads empty, because none of it is here.
    /// States needing a decision read full and are distinguished by colour
    /// rather than by length — there is no honest fraction for "you have edits
    /// that conflict".
    pub fn backup_progress(&self) -> f32 {
        match self.status {
            ProjectStatus::Syncing => self.sync_progress,
            ProjectStatus::NotDownloaded => 0.0,
            _ => 1.0,
        }
    }

    /// Whether the bar is drawn at full strength.
    ///
    /// Reserved for work in flight and for anything the person has to decide
    /// about. A project that is simply backed up gets a dimmed bar: it is
    /// still worth seeing, but it is not asking for anything.
    pub const fn backup_bar_is_prominent(&self) -> bool {
        matches!(self.status, ProjectStatus::Syncing) || self.status.needs_attention()
    }

    pub fn displayed_path(&self) -> String {
        match self.status {
            ProjectStatus::NotDownloaded => self.location.as_ref().map_or_else(
                || "Not on this computer".to_owned(),
                |location| format!("Restore location · {}", location.relative_path),
            ),
            _ => self.location.as_ref().map_or_else(
                || self.local_path.clone(),
                |location| {
                    format!(
                        "{} · Library path · {}",
                        self.local_path, location.relative_path
                    )
                },
            ),
        }
    }
}

/// Build the library from what is actually on disk.
///
/// Watched folders are scanned and individually added projects are read
/// directly; a project that appears in both is listed once. Nothing is
/// invented — an empty result means there is genuinely nothing to show yet,
/// which is the honest state before any folder has been chosen.
/// How the sidebar orders the library.
///
/// Every order is a total one: each falls back to name, and then to the
/// project's location, so a list of hundreds never reshuffles between frames
/// just because two projects tie. That matters more than it sounds — an
/// unstable sort makes the row under the pointer move as you go to click it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortOrder {
    /// Anything needing a person's attention first, then alphabetically.
    ///
    /// The default, because the list is long enough that what needs doing has
    /// to be reachable without scrolling.
    #[default]
    AttentionRequired,
    /// Most recently saved in the DAW first.
    LastModifiedLocal,
    /// Most recently backed up first.
    LastModifiedRemote,
    NameAscending,
    /// Most recently added to Auru first.
    RecentlyAdded,
}

impl SortOrder {
    /// In the order they appear in the menu.
    pub const ALL: [Self; 5] = [
        Self::LastModifiedLocal,
        Self::LastModifiedRemote,
        Self::NameAscending,
        Self::RecentlyAdded,
        Self::AttentionRequired,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::LastModifiedLocal => "Last Modified (Local)",
            Self::LastModifiedRemote => "Last Modified (Remote)",
            Self::NameAscending => "Name",
            Self::RecentlyAdded => "Recently Added",
            Self::AttentionRequired => "Attention Required",
        }
    }

    /// Short form for the sidebar header, where there is no room for the rest.
    pub const fn short_label(self) -> &'static str {
        match self {
            Self::LastModifiedLocal => "MODIFIED",
            Self::LastModifiedRemote => "BACKED UP",
            Self::NameAscending => "NAME",
            Self::RecentlyAdded => "ADDED",
            Self::AttentionRequired => "ATTENTION",
        }
    }

    /// The key persisted in the state file.
    ///
    /// Spelled out rather than derived from the variant name so renaming a
    /// variant cannot silently reset everyone's saved preference.
    pub const fn key(self) -> &'static str {
        match self {
            Self::LastModifiedLocal => "modified-local",
            Self::LastModifiedRemote => "modified-remote",
            Self::NameAscending => "name",
            Self::RecentlyAdded => "recently-added",
            Self::AttentionRequired => "attention",
        }
    }

    /// Read a persisted key. An unknown one falls back to the default rather
    /// than failing — a preference is not worth refusing to start over.
    pub fn from_key(key: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|order| order.key() == key)
            .unwrap_or_default()
    }
}

/// Newest first, with unknown times last.
///
/// `None` means we could not read a time — an unplugged drive, a project never
/// backed up — which is not the same as "long ago", so those sort to the end
/// instead of competing for the top.
fn newest_first(left: Option<SystemTime>, right: Option<SystemTime>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => right.cmp(&left),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Put `projects` in the requested order.
pub fn sort_projects(projects: &mut [Project], order: SortOrder) {
    projects.sort_by(|left, right| {
        let primary = match order {
            SortOrder::AttentionRequired => right
                .status
                .needs_attention()
                .cmp(&left.status.needs_attention()),
            SortOrder::LastModifiedLocal => newest_first(left.modified_at, right.modified_at),
            SortOrder::LastModifiedRemote => newest_first(left.backed_up_at, right.backed_up_at),
            SortOrder::NameAscending => Ordering::Equal,
            SortOrder::RecentlyAdded => right.added_at.cmp(&left.added_at),
        };
        primary
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.local_path.cmp(&right.local_path))
    });
}

/// Replace one provider's catalogue rows while preserving linked local copies.
///
/// A project found on disk wins over its provider-only row; otherwise a
/// refresh replaces stale remote metadata rather than duplicating it.
pub fn replace_provider_projects(
    projects: &mut Vec<Project>,
    provider_id: &str,
    remote: Vec<RemoteProjectSeed>,
) {
    projects.retain(|project| {
        project.live_set.is_some()
            || project
                .remote
                .as_ref()
                .is_none_or(|remote| remote.provider_id != provider_id)
    });
    let mut known_handles: HashSet<(String, String)> = projects
        .iter()
        .flat_map(|project| {
            project
                .provider_handles
                .iter()
                .map(|(provider_id, handle)| (provider_id.clone(), handle.clone()))
        })
        .collect();
    for seed in remote {
        if let Some(project) = projects.iter_mut().find(|project| {
            project
                .provider_handles
                .get(&seed.provider_id)
                .is_some_and(|handle| handle == &seed.handle)
        }) {
            project.metadata = seed.metadata.clone();
            if project.location.is_none() {
                project.location = seed.location.clone();
            }
            continue;
        }
        if !known_handles.insert((seed.provider_id.clone(), seed.handle.clone())) {
            continue;
        }
        projects.push(Project::from_remote(seed));
    }
}

pub fn load_library(state: &mut crate::state::AppState) -> Vec<Project> {
    let mut roots: Vec<(PathBuf, Option<ProjectLocation>)> = Vec::new();

    // Broad roots come first so overlapping entries preserve the whole logical
    // hierarchy rather than whichever narrower folder happened to be saved
    // first by an older version of the app.
    let mut watched_folders = state.watched_folders.iter().collect::<Vec<_>>();
    watched_folders.sort_by_key(|path| path.components().count());
    for folder in watched_folders {
        for found in discovery::scan_for_projects(folder, &discovery::ScanOptions::default()) {
            // The path that identifies this project: its folder when it owns
            // one, otherwise the project file itself.
            let root = if found.owns_its_directory() {
                found.directory().to_path_buf()
            } else {
                found.project_file().to_path_buf()
            };
            let location = portable_project_location(folder, &root);
            roots.push((root, location));
        }
    }
    roots.extend(state.projects.iter().cloned().map(|root| (root, None)));

    let mut seen = std::collections::BTreeSet::new();
    let mut projects: Vec<Project> = roots
        .into_iter()
        .filter(|(root, _)| seen.insert(root.clone()))
        .filter_map(|(root, location)| {
            let mut project = Project::read_from_disk(&root)?;
            if location.is_some() {
                project.location = location;
            }
            Some(project)
        })
        .collect();

    // "Recently added" has to mean added *to Auru*, which nothing on disk
    // records — a project's folder is as old as the music. So the first time
    // we ever see one, we note when that was.
    //
    // Persisted here rather than by the caller: there are three call sites,
    // and a stamp that is not written back makes every project look newly
    // added on every launch — a sort order that quietly means nothing.
    // Entries are never pruned, so a longer map is the only way this grows.
    let known_before = state.first_seen.len();
    for project in &mut projects {
        project.added_at = Some(state.note_first_seen(&project.local_path));
    }
    if state.first_seen.len() != known_before {
        state.save();
    }

    sort_projects(&mut projects, SortOrder::from_key(&state.sort_order));
    projects
}

#[cfg(test)]
mod tests {
    /// A small Live Set the tests build real project folders from.
    ///
    /// Deliberately a real set rather than hand-written numbers: the detail page
    /// then shows what the Ableton reader actually produces, so the two cannot
    /// drift apart unnoticed. Shaped after a real drum-and-bass project — 175 BPM
    /// in C Phrygian, hosting Serum and Ozone.
    const TEST_LIVE_SET: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
    <Ableton MajorVersion="5" MinorVersion="12.0_12049" Creator="Ableton Live 12.0.25">
      <LiveSet>
        <MainTrack><DeviceChain><Mixer>
          <Tempo><Manual Value="175" /></Tempo>
          <TimeSignature><Manual Value="201" /></TimeSignature>
        </Mixer></DeviceChain></MainTrack>
        <ScaleInformation><RootNote Value="0" /><Name Value="Phrygian" /></ScaleInformation>
        <InKey Value="true" />
        <Scenes><Scene Id="0" /><Scene Id="1" /></Scenes>
        <Tracks>
          <MidiTrack Id="1">
            <Name><EffectiveName Value="Reese" /></Name>
            <DeviceChain><DeviceChain><Devices>
              <PluginDevice><PluginDesc><Vst3PluginInfo>
                <Name Value="Serum 2" /><DeviceType Value="1" />
                <Uid><Fields.0 Value="1448297816" /><Fields.1 Value="1718833267" />
                     <Fields.2 Value="1701999981" /><Fields.3 Value="540147712" /></Uid>
              </Vst3PluginInfo></PluginDesc></PluginDevice>
              <Eq8 />
            </Devices></DeviceChain></DeviceChain>
            <MidiClip Id="0"><CurrentStart Value="0" /><CurrentEnd Value="64" /></MidiClip>
            <MidiClip Id="1"><CurrentStart Value="64" /><CurrentEnd Value="128" /></MidiClip>
          </MidiTrack>
          <MidiTrack Id="2">
            <Name><EffectiveName Value="Screech" /></Name>
            <DeviceChain><DeviceChain><Devices>
              <PluginDevice><PluginDesc><Vst3PluginInfo>
                <Name Value="Serum 2" /><DeviceType Value="1" />
                <Uid><Fields.0 Value="1448297816" /><Fields.1 Value="1718833267" />
                     <Fields.2 Value="1701999981" /><Fields.3 Value="540147712" /></Uid>
              </Vst3PluginInfo></PluginDesc></PluginDevice>
            </Devices></DeviceChain></DeviceChain>
            <MidiClip Id="0"><CurrentStart Value="128" /><CurrentEnd Value="192" /></MidiClip>
          </MidiTrack>
          <AudioTrack Id="3">
            <Name><EffectiveName Value="Break" /></Name>
            <DeviceChain><DeviceChain><Devices><Eq8 /><Reverb /></Devices></DeviceChain></DeviceChain>
            <AudioClip Id="0">
              <CurrentStart Value="0" /><CurrentEnd Value="352" />
              <SampleRef><FileRef>
                <RelativePathType Value="1" />
                <RelativePath Value="../../samples/SPLICE/break.wav" />
                <Path Value="E:/Music Production/samples/SPLICE/break.wav" />
                <OriginalFileSize Value="5907514" />
              </FileRef></SampleRef>
            </AudioClip>
          </AudioTrack>
          <AudioTrack Id="4">
            <Name><EffectiveName Value="Master Bus" /></Name>
            <DeviceChain><DeviceChain><Devices>
              <PluginDevice><PluginDesc><VstPluginInfo>
                <PlugName Value="Ozone 8 Elements" /><UniqueId Value="1517176172" />
              </VstPluginInfo></PluginDesc></PluginDevice>
            </Devices></DeviceChain></DeviceChain>
          </AudioTrack>
          <ReturnTrack Id="5">
            <Name><EffectiveName Value="A-Reverb" /></Name>
            <DeviceChain><DeviceChain><Devices><Reverb /></Devices></DeviceChain></DeviceChain>
          </ReturnTrack>
          <ReturnTrack Id="6">
            <Name><EffectiveName Value="B-Delay" /></Name>
            <DeviceChain><DeviceChain><Devices><Delay /></Devices></DeviceChain></DeviceChain>
          </ReturnTrack>
        </Tracks>
      </LiveSet>
    </Ableton>"#;

    use super::*;

    #[test]
    fn downloaded_project_should_not_need_attention() {
        assert!(!ProjectStatus::Downloaded.needs_attention());
    }

    #[test]
    fn a_provider_project_should_enter_the_library_as_restorable() {
        let head = CommitId(auru_pm::ContentHash::of(b"remote head"));
        let project = Project::from_remote(RemoteProjectSeed {
            provider_id: "studio-nas".into(),
            provider_name: "Studio NAS".into(),
            handle: "night-drive".into(),
            head,
            name: "Night Drive".into(),
            file_name: "Night Drive.als".into(),
            format: ProjectFormat::AbletonLiveSet,
            metadata: ProjectMetadata::default(),
            location: Some(ProjectLocation {
                relative_path: "Ableton/Projects/Night Drive Project".into(),
            }),
            updated_at: 1_750_000_000,
            info: None,
        });

        assert_eq!(project.status, ProjectStatus::NotDownloaded);
        assert_eq!(project.status.action(), ProjectAction::Restore);
        assert!(project.live_set.is_none());
        assert_eq!(
            project
                .provider_handles
                .get("studio-nas")
                .map(String::as_str),
            Some("night-drive")
        );
        assert_eq!(
            project.remote.as_ref().map(|remote| remote.head),
            Some(head)
        );
    }

    #[test]
    fn a_provider_refresh_should_replace_stale_rows_without_duplicates() {
        let seed = |name: &str, head: &[u8]| RemoteProjectSeed {
            provider_id: "studio-nas".into(),
            provider_name: "Studio NAS".into(),
            handle: "night-drive".into(),
            head: CommitId(auru_pm::ContentHash::of(head)),
            name: name.into(),
            file_name: format!("{name}.als"),
            format: ProjectFormat::AbletonLiveSet,
            metadata: ProjectMetadata::default(),
            location: None,
            updated_at: 1_750_000_000,
            info: None,
        };
        let old_head = CommitId(auru_pm::ContentHash::of(b"old head"));
        let mut projects = vec![Project::from_remote(seed("Old Name", b"old head"))];

        replace_provider_projects(
            &mut projects,
            "studio-nas",
            vec![seed("Night Drive", b"new head")],
        );

        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "Night Drive");
        assert_ne!(
            projects[0].remote.as_ref().map(|remote| remote.head),
            Some(old_head)
        );
    }

    #[test]
    fn a_provider_refresh_should_apply_metadata_to_a_linked_local_project() {
        let metadata = ProjectMetadata {
            genre: Some("Ambient".to_owned()),
            tags: vec!["mastered".to_owned()],
        };
        let seed = RemoteProjectSeed {
            provider_id: "studio-nas".into(),
            provider_name: "Studio NAS".into(),
            handle: "night-drive".into(),
            head: CommitId(auru_pm::ContentHash::of(b"head")),
            name: "Night Drive".into(),
            file_name: "Night Drive.als".into(),
            format: ProjectFormat::AbletonLiveSet,
            metadata: metadata.clone(),
            location: None,
            updated_at: 1_750_000_000,
            info: None,
        };
        let mut local = Project::from_remote(seed.clone());
        local.live_set = Some(PathBuf::from("/music/Night Drive.als"));
        local.metadata = ProjectMetadata::default();
        let mut projects = vec![local];

        replace_provider_projects(&mut projects, "studio-nas", vec![seed]);

        assert_eq!(projects[0].metadata, metadata);
    }

    fn detail() -> ProjectDetail {
        ProjectDetail {
            tempo: Some(175.0),
            time_signature: Some((4, 4)),
            key: Some("C Phrygian".to_owned()),
            in_key: true,
            tracks_total: 16,
            tracks_midi: 9,
            tracks_audio: 4,
            tracks_return: 2,
            clip_count: 53,
            bars: Some(88.0),
            live_version: Some("Ableton Live 12.0.25".to_owned()),
            files_total: 11,
            files_gathered: 5,
        }
    }

    #[test]
    fn detail_should_read_the_way_a_musician_would_say_it() {
        let detail = detail();
        assert_eq!(detail.tempo_line(), "175 BPM · 4/4");
        assert_eq!(detail.key_line(), "C Phrygian · in key");
        assert_eq!(
            detail.tracks_line(),
            "16 tracks · 9 MIDI · 4 audio · 2 returns"
        );
        assert_eq!(detail.length_line(), "88 bars · 53 clips");
        assert_eq!(detail.files_line(), "11 files · 5 gathered in");
    }

    #[test]
    fn detail_should_omit_what_the_project_never_declared() {
        // Showing "0 BPM" or "C Major" for a project that said neither would
        // be inventing information about someone's music.
        let empty = ProjectDetail::default();
        assert_eq!(empty.tempo_line(), "—");
        assert_eq!(empty.key_line(), "—");
        assert_eq!(empty.tracks_line(), "—");
        assert_eq!(empty.length_line(), "—");
        assert_eq!(empty.files_line(), "—");
        assert_eq!(empty.made_with(), "Ableton Live");
    }

    #[test]
    fn a_single_track_should_not_be_pluralized() {
        let detail = ProjectDetail {
            tracks_total: 1,
            tracks_midi: 1,
            clip_count: 1,
            bars: Some(1.0),
            ..ProjectDetail::default()
        };
        assert_eq!(detail.tracks_line(), "1 track · 1 MIDI");
        assert_eq!(detail.length_line(), "1 bar · 1 clip");
    }

    #[test]
    fn a_fractional_tempo_should_keep_its_precision() {
        let detail = ProjectDetail {
            tempo: Some(174.5),
            ..ProjectDetail::default()
        };
        assert_eq!(detail.tempo_line(), "174.50 BPM");
    }

    #[test]
    fn a_plugin_used_once_should_be_described_in_the_singular() {
        let plugin = MissingPlugin {
            name: "Ozone 8 Elements".to_owned(),
            vendor: "iZotope".to_owned(),
            format: "VST2".to_owned(),
            instances: 1,
            link: None,
        };
        assert_eq!(plugin.detail_line(), "iZotope · VST2 · used once");
    }

    #[test]
    fn only_plugins_absent_from_this_computer_should_reach_the_list() {
        // Installed and Live-bundled plugins are not the user's problem, and
        // listing them would bury the ones that are.
        use auru_pm::{PluginFormat, PluginId};

        let make = |availability| auru_pm::ResolvedPlugin {
            id: PluginId::Vst2 { unique_id: 1 },
            name: "Serum".to_owned(),
            vendor: "Xfer Records".to_owned(),
            format: PluginFormat::Vst2,
            instances: 1,
            availability,
            source: None,
            notes: None,
        };

        assert!(
            MissingPlugin::from_resolved(&make(PluginAvailability::NotOnThisComputer)).is_some()
        );
        assert!(MissingPlugin::from_resolved(&make(PluginAvailability::Installed)).is_none());
        assert!(MissingPlugin::from_resolved(&make(PluginAvailability::BundledWithDaw)).is_none());
        assert!(MissingPlugin::from_resolved(&make(PluginAvailability::Unknown)).is_none());
    }

    #[test]
    fn detail_should_be_built_from_a_real_project_summary() {
        // The wiring that matters: what the detail page shows is what a
        // commit recorded, not a parallel description of it.
        let xml = br#"<Ableton MajorVersion="5" Creator="Ableton Live 12.0.25"><LiveSet>
            <MainTrack><DeviceChain><Mixer>
              <Tempo><Manual Value="175" /></Tempo>
              <TimeSignature><Manual Value="201" /></TimeSignature>
            </Mixer></DeviceChain></MainTrack>
            <ScaleInformation><RootNote Value="0" /><Name Value="Phrygian" /></ScaleInformation>
            <InKey Value="true" />
            <Tracks>
              <MidiTrack Id="1"><Name><EffectiveName Value="Reese" /></Name></MidiTrack>
              <AudioTrack Id="2"><Name><EffectiveName Value="Break" /></Name></AudioTrack>
            </Tracks>
          </LiveSet></Ableton>"#;
        let snapshot =
            auru_pm::ProjectSnapshot::from_source_bytes(ProjectFormat::AbletonLiveSet, xml)
                .expect("normalize");
        let info = ProjectInfo::from_snapshot_bytes(snapshot.as_bytes()).expect("summary");

        let detail = ProjectDetail::from_project_info(&info).expect("detail");
        assert_eq!(detail.tempo_line(), "175 BPM · 4/4");
        assert_eq!(detail.key_line(), "C Phrygian · in key");
        assert_eq!(detail.tracks_line(), "2 tracks · 1 MIDI · 1 audio");
        assert_eq!(detail.made_with(), "Ableton Live 12.0.25");
    }

    /// Write a minimal Ableton project folder at `project`.
    fn write_project_folder_at(project: &Path) {
        let gzipped = ProjectSnapshot::from_source_bytes(
            ProjectFormat::AbletonLiveSet,
            TEST_LIVE_SET.as_bytes(),
        )
        .expect("normalize")
        .restore_bytes()
        .expect("gzip");
        std::fs::create_dir_all(project.join("Ableton Project Info")).expect("create dirs");
        std::fs::write(project.join("Night Drive.als"), &gzipped).expect("write set");
    }

    /// Write a minimal Ableton project folder and return its root.
    fn write_project_folder(root: &Path) -> std::path::PathBuf {
        let gzipped = ProjectSnapshot::from_source_bytes(
            ProjectFormat::AbletonLiveSet,
            TEST_LIVE_SET.as_bytes(),
        )
        .expect("normalize")
        .restore_bytes()
        .expect("gzip");
        let project = root.join("Night Drive Project");
        std::fs::create_dir_all(project.join("Ableton Project Info")).expect("create dirs");
        std::fs::write(project.join("Night Drive.als"), &gzipped).expect("write set");
        project
    }

    #[test]
    fn importing_an_ableton_folder_should_read_the_project_inside_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = write_project_folder(temp.path());

        let project =
            import_project(ImportKind::AbletonLiveSet, &project_root).expect("import the folder");

        assert_eq!(project.name, "Night Drive");
        assert_eq!(project.format, ProjectFormat::AbletonLiveSet);
        let detail = project.detail.as_ref().expect("detail read from the set");
        assert_eq!(detail.tempo_line(), "175 BPM · 4/4");
        assert_eq!(detail.key_line(), "C Phrygian · in key");
    }

    #[test]
    fn importing_the_als_inside_a_folder_should_reach_the_same_project() {
        // Someone may well pick the file rather than the folder; both are the
        // same project and should not produce two library entries.
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = write_project_folder(temp.path());

        let from_folder =
            import_project(ImportKind::AbletonLiveSet, &project_root).expect("from folder");
        let from_file = import_project(
            ImportKind::AbletonLiveSet,
            &project_root.join("Night Drive.als"),
        )
        .expect("from file");

        assert_eq!(
            from_folder.id, from_file.id,
            "both routes must identify one project, so re-adding refreshes it"
        );
    }

    #[test]
    fn an_imported_project_should_start_as_not_backed_up() {
        // It exists only on this computer until it is pushed, and the status
        // should say so rather than implying it is safe.
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = write_project_folder(temp.path());

        let project = import_project(ImportKind::AbletonLiveSet, &project_root).expect("import");
        assert_eq!(
            project.status,
            ProjectStatus::OutOfSync(SyncDirection::LocalAhead)
        );
        assert!(project.status.needs_attention());
        assert_eq!(project.safe_version, "not backed up yet");
        assert!(project.versions.is_empty(), "no history until it is pushed");
    }

    #[test]
    fn importing_the_wrong_kind_should_explain_itself() {
        // Picking a .als from the DAWproject menu is an easy mistake; the
        // message has to name what was actually found.
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = write_project_folder(temp.path());

        let error = import_project(
            ImportKind::Dawproject,
            &project_root.join("Night Drive.als"),
        )
        .expect_err("a Live Set is not a DAWproject");

        assert!(error.contains("Ableton Live Set"), "{error}");
        assert!(error.contains("DAWproject"), "{error}");
    }

    #[test]
    fn importing_something_unreadable_should_fail_kindly() {
        let temp = tempfile::tempdir().expect("tempdir");
        let bogus = temp.path().join("holiday-photo.jpg");
        std::fs::write(&bogus, b"not a project").expect("write");

        let error = import_project(ImportKind::AbletonLiveSet, &bogus)
            .expect_err("a JPEG is not a project");
        assert!(
            error.starts_with("Couldn't read holiday-photo.jpg"),
            "the message should name the file the person picked: {error}"
        );
    }

    #[test]
    fn importing_a_folder_with_no_project_should_say_so() {
        let temp = tempfile::tempdir().expect("tempdir");
        let empty = temp.path().join("Some Folder");
        std::fs::create_dir_all(&empty).expect("create");

        let error =
            import_project(ImportKind::AbletonLiveSet, &empty).expect_err("nothing to import");
        assert!(error.contains("doesn't contain a project"), "{error}");
    }

    #[test]
    fn watching_a_folder_should_find_the_projects_inside_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let library = temp.path().join("Ableton Projects");
        for name in ["110 riddim Project", "dunno yet-1 Project"] {
            write_project_folder_at(&library.join(name));
        }

        let watched = WatchedFolder::scan(&library);
        assert_eq!(watched.project_count(), 2);
        assert!(watched.total_bytes() > 0, "sizes should be measured");

        let names: Vec<&str> = watched
            .projects
            .iter()
            .map(|project| project.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["Night Drive", "Night Drive"],
            "each set's own name"
        );
    }

    #[test]
    fn watching_a_library_root_should_keep_each_nested_project_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_project_folder_at(
            &temp
                .path()
                .join("Ableton")
                .join("Projects")
                .join("Night Drive Project"),
        );
        write_bitwig_project_at(
            &temp
                .path()
                .join("Bitwig Projects")
                .join("Broken Glass")
                .join("Broken Glass.bwproject"),
        );

        let watched = WatchedFolder::scan(temp.path());
        let paths = watched
            .projects
            .iter()
            .map(FoundProject::library_path)
            .collect::<Vec<_>>();

        assert_eq!(
            paths,
            [
                "Ableton/Projects/Night Drive Project",
                "Bitwig Projects/Broken Glass/Broken Glass.bwproject"
            ]
        );
    }

    #[test]
    fn loading_a_watched_library_should_attach_the_restore_location() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_project_folder_at(
            &temp
                .path()
                .join("Ableton")
                .join("Projects")
                .join("Night Drive Project"),
        );
        let mut state = crate::state::AppState::default();
        state.watch(temp.path());

        let projects = load_library(&mut state);

        assert_eq!(
            projects[0]
                .location
                .as_ref()
                .map(|location| location.relative_path.as_str()),
            Some("Ableton/Projects/Night Drive Project")
        );
    }

    #[test]
    fn watching_a_folder_with_no_projects_should_find_nothing() {
        // Reported honestly rather than as an error: pointing at the wrong
        // folder is an easy mistake and not a failure.
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(temp.path().join("Holiday Photos")).expect("create");

        let watched = WatchedFolder::scan(temp.path());
        assert_eq!(watched.project_count(), 0);
        assert_eq!(watched.total_bytes(), 0);
    }

    #[test]
    fn watching_should_not_count_a_projects_own_autosaves() {
        // The `Backup` folder inside a project holds Live's autosaves. Each
        // would otherwise look like a project of its own.
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("Song Project");
        write_project_folder_at(&project);
        std::fs::create_dir_all(project.join("Backup")).expect("create");
        std::fs::write(
            project.join("Backup/Song [2026-01-01 000000].als"),
            b"autosave",
        )
        .expect("write");

        assert_eq!(WatchedFolder::scan(temp.path()).project_count(), 1);
    }

    #[test]
    fn sizes_should_be_written_the_way_a_musician_reads_them() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(2_048), "2 KB");
        assert_eq!(format_bytes(5_242_880), "5 MB");
        assert_eq!(format_bytes(1_288_490_189), "1.2 GB");
    }

    #[test]
    fn a_project_never_backed_up_should_say_so_and_ask_for_attention() {
        // The starting state for everything Auru finds, and the one that
        // matters: the work exists in exactly one place.
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("Song Project");
        write_project_folder_at(&root);

        let project = Project::read_from_disk(&root).expect("a project");
        assert_eq!(project.status, ProjectStatus::NeverBackedUp);
        assert!(project.status.needs_attention());
        assert_eq!(project.status.action(), ProjectAction::Push);
        assert_eq!(project.list_status(), "only on this computer");
        assert!(
            project.status_explanation().contains("lost or stolen"),
            "it should say plainly what is at stake"
        );
    }

    #[test]
    fn a_project_with_a_recorded_backup_should_not_read_as_never_backed_up() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("Song Project");
        write_project_folder_at(&root);

        // A sidecar recording a commit is what "has been backed up" means.
        let sidecar_path = auru_pm::sidecar_path_for(&root.join("Night Drive.als"));
        let sidecar = Sidecar {
            primary: Some("studio-nas".into()),
            provider_handles: std::collections::BTreeMap::from([(
                "studio-nas".into(),
                "night-drive".into(),
            )]),
            local_head: Some(auru_pm::CommitId(auru_pm::ContentHash::of(b"a commit"))),
            ..Sidecar::default()
        };
        sidecar.save(&sidecar_path).expect("write sidecar");

        let project = Project::read_from_disk(&root).expect("a project");
        assert_ne!(project.status, ProjectStatus::NeverBackedUp);
        assert_eq!(
            project
                .provider_handles
                .get("studio-nas")
                .map(String::as_str),
            Some("night-drive")
        );
    }

    #[test]
    fn a_local_project_should_match_every_provider_handle_in_its_sidecar() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("Night Drive Project");
        write_project_folder_at(&root);
        let project_file = root.join("Night Drive.als");
        let sidecar_path = sidecar_path_for(&project_file);
        Sidecar {
            primary: Some("auru-cloud".into()),
            provider_handles: std::collections::BTreeMap::from([
                ("auru-cloud".into(), "cloud-night-drive".into()),
                ("studio-nas".into(), "nas-night-drive".into()),
            ]),
            local_head: Some(auru_pm::CommitId(auru_pm::ContentHash::of(b"a commit"))),
            ..Sidecar::default()
        }
        .save(&sidecar_path)
        .expect("sidecar");

        let project = Project::read_from_disk(&root).expect("a project");
        std::fs::remove_file(sidecar_path).expect("prove matching uses cached handles");

        assert_eq!(
            project
                .provider_handles
                .get("auru-cloud")
                .map(String::as_str),
            Some("cloud-night-drive")
        );
        assert_eq!(
            project
                .provider_handles
                .get("studio-nas")
                .map(String::as_str),
            Some("nas-night-drive")
        );
    }

    #[test]
    fn reading_a_project_should_not_open_its_live_set() {
        // The library lists hundreds of projects; detail waits until one is
        // actually selected.
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("Song Project");
        write_project_folder_at(&root);

        let project = Project::read_from_disk(&root).expect("a project");
        assert!(project.detail.is_none(), "detail is deferred");
        assert!(project.missing_plugins.is_empty());
        assert!(project.live_set.is_some(), "but we know where to find it");
    }

    #[test]
    fn loading_detail_should_fill_in_what_the_project_says() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("Song Project");
        write_project_folder_at(&root);

        let mut project = Project::read_from_disk(&root).expect("a project");
        let live_set = project.live_set.clone().expect("a live set");
        project.apply_detail(Project::detail_for(&live_set));

        let detail = project.detail.as_ref().expect("detail");
        assert_eq!(detail.tempo_line(), "175 BPM · 4/4");
        assert_eq!(detail.key_line(), "C Phrygian · in key");
    }

    #[test]
    fn a_folder_that_is_not_a_project_should_be_skipped() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(temp.path().join("Holiday Photos")).expect("create");
        assert!(Project::read_from_disk(&temp.path().join("Holiday Photos")).is_none());
    }

    #[test]
    fn the_library_should_come_from_watched_folders_and_added_projects() {
        let temp = tempfile::tempdir().expect("tempdir");
        let library = temp.path().join("Ableton Projects");
        write_project_folder_at(&library.join("Bass Thing Project"));
        write_project_folder_at(&library.join("Another Project"));
        let separate = temp.path().join("elsewhere/One Off Project");
        write_project_folder_at(&separate);

        let mut state = crate::state::AppState::default();
        state.watch(&library);
        state.add_project(&separate);

        let projects = load_library(&mut state);
        assert_eq!(projects.len(), 3);
    }

    #[test]
    fn a_project_reached_two_ways_should_be_listed_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let library = temp.path().join("Projects");
        let project = library.join("Song Project");
        write_project_folder_at(&project);

        let mut state = crate::state::AppState::default();
        state.watch(&library);
        // Adding it explicitly as well must not duplicate the row.
        state.projects.push(project);

        assert_eq!(load_library(&mut state).len(), 1);
    }

    #[test]
    fn an_empty_library_should_simply_be_empty() {
        // Before any folder is chosen there is genuinely nothing to show, and
        // inventing something would be worse than an empty list.
        let projects = load_library(&mut crate::state::AppState::default());
        assert!(projects.is_empty());
    }

    #[test]
    fn projects_needing_attention_should_sort_first() {
        let temp = tempfile::tempdir().expect("tempdir");
        let library = temp.path().join("Projects");
        write_project_folder_at(&library.join("Zebra Project"));
        let backed_up = library.join("Alpha Project");
        write_project_folder_at(&backed_up);

        let sidecar_path = auru_pm::sidecar_path_for(&backed_up.join("Night Drive.als"));
        let sidecar = Sidecar {
            local_head: Some(auru_pm::CommitId(auru_pm::ContentHash::of(b"a commit"))),
            ..Sidecar::default()
        };
        sidecar.save(&sidecar_path).expect("write sidecar");

        let mut state = crate::state::AppState::default();
        state.watch(&library);
        let projects = load_library(&mut state);

        assert!(
            projects[0].status.needs_attention(),
            "what needs doing comes first, ahead of alphabetical order"
        );
    }

    #[test]
    fn a_transfer_should_show_only_confirmed_completion() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("Song Project");
        write_project_folder_at(&root);
        let mut project = Project::read_from_disk(&root).expect("a project");

        assert!(project.begin_transfer());
        assert_eq!(project.backup_progress(), 0.0);

        project.finish_transfer(Vec::new());
        assert_eq!(project.status, ProjectStatus::Downloaded);
        assert_eq!(project.backup_progress(), 1.0);
        assert!(!project.backup_bar_is_prominent(), "backed up is quiet");
    }

    #[test]
    fn a_project_never_backed_up_should_stand_out() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("Song Project");
        write_project_folder_at(&root);
        let project = Project::read_from_disk(&root).expect("a project");
        assert!(project.backup_bar_is_prominent());
    }

    #[test]
    fn search_should_match_the_name_file_genre_and_tags() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("Song Project");
        write_project_folder_at(&root);
        let mut project = Project::read_from_disk(&root).expect("a project");
        project.metadata = ProjectMetadata {
            genre: Some("Drum & Bass".to_owned()),
            tags: vec!["work in progress".to_owned(), "collab".to_owned()],
        };

        assert!(project.matches_search(""), "an empty box shows everything");
        assert!(project.matches_search("   "), "and so does whitespace");
        assert!(
            project.matches_search("night"),
            "case-insensitive on the name"
        );
        assert!(project.matches_search("NIGHT DRIVE"));
        assert!(project.matches_search(".als"), "and matches the file name");
        assert!(project.matches_search("drum & bass"));
        assert!(project.matches_search("COLLAB"));
        assert!(!project.matches_search("something else"));
    }

    #[test]
    fn library_filters_should_or_values_inside_each_facet() {
        let mut project = sortable("Night Drive", ProjectStatus::Downloaded);
        project.metadata = ProjectMetadata {
            genre: Some("Drum & Bass, Jungle".to_owned()),
            tags: vec!["collab".to_owned()],
        };
        project.detail = Some(ProjectDetail {
            tempo: Some(174.5),
            ..ProjectDetail::default()
        });

        let matches = project.matches_library_filters(
            "night",
            &["Ambient".to_owned(), "jungle".to_owned()],
            &["mastered".to_owned(), "COLLAB".to_owned()],
            Some(BpmFilter::Range { min: 170, max: 180 }),
        );

        assert!(matches);
    }

    #[test]
    fn exact_bpm_should_match_fractional_tempos_that_round_to_it() {
        assert!(BpmFilter::Exact(175).matches(Some(174.5)));
    }

    #[test]
    fn bpm_filter_should_not_guess_when_a_project_has_no_tempo() {
        assert!(!BpmFilter::Range { min: 80, max: 160 }.matches(None));
    }

    #[test]
    fn filter_options_should_be_sorted_and_deduplicated_case_insensitively() {
        let mut first = sortable("One", ProjectStatus::Downloaded);
        first.metadata = ProjectMetadata {
            genre: Some("Drum & Bass, Jungle".to_owned()),
            tags: vec!["Collab".to_owned(), "WIP".to_owned()],
        };
        let mut second = sortable("Two", ProjectStatus::Downloaded);
        second.metadata = ProjectMetadata {
            genre: Some("drum & bass".to_owned()),
            tags: vec!["collab".to_owned(), "Club".to_owned()],
        };

        assert_eq!(
            library_filter_options(&[first, second]),
            LibraryFilterOptions {
                genres: vec!["Drum & Bass".to_owned(), "Jungle".to_owned()],
                tags: vec!["Club".to_owned(), "Collab".to_owned(), "WIP".to_owned()],
            }
        );
    }

    /// A project carrying only the fields sorting looks at.
    ///
    /// Built by hand rather than read from disk: these tests are about the
    /// comparison, and giving each case a real folder with a controlled
    /// modification time would test the filesystem instead.
    fn sortable(name: &str, status: ProjectStatus) -> Project {
        Project {
            id: format!("test:{name}"),
            name: name.to_owned(),
            file_name: format!("{name}.als"),
            local_path: format!("/music/{name}"),
            size: String::new(),
            format: ProjectFormat::AbletonLiveSet,
            status,
            last_activity: String::new(),
            safe_version: String::new(),
            local_inventory: String::new(),
            versions: Vec::new(),
            sync_progress: 0.0,
            modified_at: None,
            backed_up_at: None,
            added_at: None,
            live_set: None,
            remote: None,
            provider_handles: BTreeMap::new(),
            metadata: ProjectMetadata::default(),
            location: None,
            detail: None,
            missing_plugins: Vec::new(),
        }
    }

    fn names(projects: &[Project]) -> Vec<&str> {
        projects
            .iter()
            .map(|project| project.name.as_str())
            .collect()
    }

    #[test]
    fn sorting_by_name_should_ignore_case() {
        let mut projects = vec![
            sortable("zebra", ProjectStatus::Downloaded),
            sortable("Apple", ProjectStatus::Downloaded),
            sortable("banana", ProjectStatus::Downloaded),
        ];
        sort_projects(&mut projects, SortOrder::NameAscending);
        assert_eq!(names(&projects), ["Apple", "banana", "zebra"]);
    }

    #[test]
    fn sorting_by_attention_should_put_what_needs_doing_first() {
        let mut projects = vec![
            sortable("safe", ProjectStatus::Downloaded),
            sortable("at risk", ProjectStatus::NeverBackedUp),
        ];
        sort_projects(&mut projects, SortOrder::AttentionRequired);
        assert_eq!(names(&projects), ["at risk", "safe"]);
    }

    #[test]
    fn sorting_by_time_should_put_the_newest_first() {
        let old = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000);
        let new = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(2_000);

        let mut projects = vec![
            sortable("older", ProjectStatus::Downloaded),
            sortable("newer", ProjectStatus::Downloaded),
        ];
        projects[0].modified_at = Some(old);
        projects[1].modified_at = Some(new);

        sort_projects(&mut projects, SortOrder::LastModifiedLocal);
        assert_eq!(names(&projects), ["newer", "older"]);
    }

    #[test]
    fn a_project_with_no_time_should_sort_last_not_first() {
        // An unplugged drive gives no modification time, and a project never
        // backed up has no backup time. Treating either as 1970 would file
        // them under "oldest", which reads as information we do not have.
        let mut projects = vec![
            sortable("unknown", ProjectStatus::Downloaded),
            sortable("ancient", ProjectStatus::Downloaded),
        ];
        projects[0].backed_up_at = None;
        projects[1].backed_up_at = Some(SystemTime::UNIX_EPOCH);

        sort_projects(&mut projects, SortOrder::LastModifiedRemote);
        assert_eq!(names(&projects), ["ancient", "unknown"]);
    }

    #[test]
    fn every_order_should_be_total_so_the_list_cannot_reshuffle() {
        // Ties are the common case: a first scan stamps every project with the
        // same added-at second. If that left the order undefined, rows would
        // move under the pointer between frames.
        let ordered = |order| {
            let mut projects = vec![
                sortable("b", ProjectStatus::Downloaded),
                sortable("a", ProjectStatus::Downloaded),
                sortable("c", ProjectStatus::Downloaded),
            ];
            for project in &mut projects {
                project.added_at = Some(42);
            }
            sort_projects(&mut projects, order);
            names(&projects).join(",")
        };

        for order in SortOrder::ALL {
            assert_eq!(ordered(order), "a,b,c", "{order:?} left ties unresolved");
        }
    }

    #[test]
    fn a_saved_sort_order_should_survive_a_restart() {
        for order in SortOrder::ALL {
            assert_eq!(SortOrder::from_key(order.key()), order);
        }
    }

    #[test]
    fn an_unreadable_sort_order_should_fall_back_rather_than_fail() {
        // Written by a newer build, or hand-edited. A preference is not worth
        // refusing to show someone their library over.
        assert_eq!(SortOrder::from_key("by-vibes"), SortOrder::default());
        assert_eq!(SortOrder::from_key(""), SortOrder::AttentionRequired);
    }

    #[test]
    fn a_rescan_should_not_make_old_projects_look_newly_added() {
        // The failure this guards is silent: if the first-seen stamp is not
        // kept across loads, every project is "added just now" every time and
        // Recently Added degenerates to alphabetical without any error.
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("Song Project");
        write_project_folder_at(&root);

        let mut state = crate::state::AppState::default();
        state.projects.push(root.clone());

        let first = load_library(&mut state);
        let added_at = first[0].added_at.expect("stamped on the first load");

        let again = load_library(&mut state);
        assert_eq!(
            again[0].added_at,
            Some(added_at),
            "a second scan re-dated a project that was already known"
        );
    }

    #[test]
    fn first_seen_should_keep_the_earliest_time_it_saw_a_project() {
        // Called on every library load, so it must record when a project was
        // *first* found, not when it was last scanned.
        let mut state = crate::state::AppState::default();
        let first = state.note_first_seen("/music/Song Project");
        let again = state.note_first_seen("/music/Song Project");
        assert_eq!(first, again, "a rescan is not a re-add");
        assert_eq!(state.first_seen.len(), 1);
    }

    #[test]
    fn every_import_kind_should_ask_for_the_right_thing() {
        // Only Ableton keeps its project in a folder; offering directories for
        // the others would invite picking something that cannot work.
        assert!(ImportKind::AbletonLiveSet.accepts_directories());
        assert!(!ImportKind::BitwigProject.accepts_directories());
        assert!(!ImportKind::Dawproject.accepts_directories());
        assert!(!ImportKind::AuruProject.accepts_directories());

        for kind in ImportKind::ALL {
            assert!(!kind.label().is_empty());
            assert!(!kind.prompt().is_empty());
        }
    }

    /// A minimal `.flp` on disk, at `path`.
    fn write_flp_at(path: &Path, channels: u16) {
        use auru_pm::flstudio::{Event, Header, Stream};
        let bytes = Stream {
            header: Header {
                format: 0,
                channels,
                ppq: 96,
            },
            events: vec![
                Event::new(199, b"20.5.0.1142\0".to_vec()),
                Event::new(156, 174_000u32.to_le_bytes()),
                Event::new(17, [4]),
                Event::new(18, [4]),
            ],
        }
        .encode();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create dirs");
        }
        std::fs::write(path, bytes).expect("write flp");
    }

    fn write_bitwig_project_at(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create dirs");
        }
        std::fs::write(path, b"BtWg0003000200ba\0project").expect("write bwproject");
    }

    #[test]
    fn an_fl_project_should_be_read_from_disk_like_any_other() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("Doom.flp");
        write_flp_at(&path, 12);

        let project = Project::read_from_disk(&path).expect("a project");
        assert_eq!(project.format, ProjectFormat::FlStudio);
        assert_eq!(project.name, "Doom");
        assert_eq!(project.file_name, "Doom.flp");
        assert_eq!(project.format_label(), "FL STUDIO PROJECT");
        assert!(project.open_label().contains("FL STUDIO"));
    }

    #[test]
    fn a_bitwig_project_should_be_read_from_disk_like_any_other() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("Song.bwproject");
        write_bitwig_project_at(&path);

        let project = Project::read_from_disk(&path).expect("a project");

        assert_eq!(project.format, ProjectFormat::BitwigProject);
    }

    #[test]
    fn two_fl_projects_in_one_folder_should_be_two_projects() {
        // The trap the whole design turns on. Keying identity on the folder —
        // which is right for Ableton — would make the second silently replace
        // the first, and one of them would vanish from the library.
        let temp = tempfile::tempdir().expect("tempdir");
        write_flp_at(&temp.path().join("One.flp"), 2);
        write_flp_at(&temp.path().join("Two.flp"), 3);

        let mut state = crate::state::AppState::default();
        state.watch(temp.path());

        let projects = load_library(&mut state);
        assert_eq!(projects.len(), 2);
        assert_ne!(projects[0].id, projects[1].id);
    }

    #[test]
    fn an_fl_project_should_not_be_measured_by_the_folder_it_sits_in() {
        // An `.flp` saved into a downloads folder does not own it. Measuring
        // the folder would report someone's whole Downloads directory as the
        // size of one project — and on a paid plan that is a bill.
        let temp = tempfile::tempdir().expect("tempdir");
        write_flp_at(&temp.path().join("Song.flp"), 2);
        std::fs::write(temp.path().join("unrelated.bin"), vec![0u8; 5_000_000]).expect("write");

        let folder = WatchedFolder::scan(temp.path());
        assert_eq!(folder.projects.len(), 1);
        assert!(
            folder.projects[0].bytes < 1_000,
            "measured {} bytes, which is the folder rather than the project",
            folder.projects[0].bytes
        );
    }

    #[test]
    fn a_watched_folder_should_find_both_daws_together() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_project_folder_at(&temp.path().join("Live Song Project"));
        write_flp_at(&temp.path().join("FL Song.flp"), 2);

        let folder = WatchedFolder::scan(temp.path());
        let names: Vec<&str> = folder
            .projects
            .iter()
            .map(|project| project.name.as_str())
            .collect();
        assert_eq!(names.len(), 2, "found {names:?}");
        assert!(names.contains(&"FL Song"));
    }

    #[test]
    fn fl_detail_should_come_from_the_real_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("Doom.flp");
        write_flp_at(&path, 12);

        let loaded = Project::detail_for(&path);
        let detail = loaded.detail.expect("detail");
        assert_eq!(detail.tempo, Some(174.0));
        assert_eq!(detail.time_signature, Some((4, 4)));
        assert_eq!(detail.tracks_total, 12, "FL counts channels, not tracks");
        assert_eq!(
            detail.key, None,
            "FL records no project-wide key; inventing one would be a lie"
        );
    }

    #[test]
    fn dawproject_detail_should_come_from_the_real_file() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/auru-pm/tests/fixtures/interchange/oracle-midi.dawproject");

        let loaded = Project::detail_for(&path);
        let detail = loaded.detail.expect("detail");
        assert_eq!(detail.tempo, Some(123.0));
        assert_eq!(detail.time_signature, Some((5, 4)));
        assert_eq!(detail.tracks_total, 1, "the master is not a song track");
        assert_eq!(detail.tracks_midi, 1);
        assert_eq!(detail.clip_count, 1);
        assert_eq!(detail.made_with(), "Auru 0.0.1");
        assert_eq!(
            detail.key, None,
            "DAWproject 1.0 has no project-wide key field"
        );
    }

    #[test]
    fn every_import_kind_should_have_its_own_action_and_prompt() {
        // Adding a DAW means adding a variant and nothing else; this catches a
        // variant that was added without being given a menu line.
        for kind in ImportKind::ALL {
            assert!(!kind.label().is_empty());
            assert!(!kind.prompt().is_empty());
        }
        assert_eq!(ImportKind::FlStudio.format(), ProjectFormat::FlStudio);
        assert!(
            !ImportKind::FlStudio.accepts_directories(),
            "an FL project is a file; offering a folder invites picking one that cannot work"
        );
    }

    #[test]
    fn a_format_without_readable_detail_should_yield_none() {
        let info = ProjectInfo {
            schema: auru_pm::PROJECT_INFO_SCHEMA,
            format: ProjectFormat::Auru,
            ableton: None,
            flstudio: None,
            dawproject: None,
        };
        assert!(ProjectDetail::from_project_info(&info).is_none());
    }
}

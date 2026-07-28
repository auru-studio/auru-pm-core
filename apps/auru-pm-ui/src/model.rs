use auru_pm::{
    PluginAvailability, PluginSearchPaths, ProjectFormat, ProjectInfo, ProjectSnapshot,
    ResolvedPlugin, ableton, plugin_registry,
};

/// A small Live Set the demo projects are read from.
///
/// Deliberately a real set rather than hand-written numbers: the detail page
/// then shows what the Ableton reader actually produces, so the two cannot
/// drift apart unnoticed. Shaped after a real drum-and-bass project — 175 BPM
/// in C Phrygian, hosting Serum and Ozone.
const DEMO_LIVE_SET: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncDirection {
    LocalAhead,
    UpstreamAhead,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectStatus {
    NotDownloaded,
    Downloaded,
    Syncing,
    OutOfSync(SyncDirection),
    Conflicted,
}

impl ProjectStatus {
    pub const fn label(self) -> &'static str {
        match self {
            Self::NotDownloaded => "Not downloaded",
            Self::Downloaded => "Downloaded",
            Self::Syncing => "Syncing",
            Self::OutOfSync(SyncDirection::LocalAhead) => "Out of sync · local ahead",
            Self::OutOfSync(SyncDirection::UpstreamAhead) => "Out of sync · upstream ahead",
            Self::Conflicted => "Conflict",
        }
    }

    pub const fn action(self) -> ProjectAction {
        match self {
            Self::NotDownloaded => ProjectAction::Download,
            Self::Downloaded => ProjectAction::Open,
            Self::Syncing => ProjectAction::None,
            Self::OutOfSync(SyncDirection::LocalAhead) => ProjectAction::Push,
            Self::OutOfSync(SyncDirection::UpstreamAhead) => ProjectAction::Pull,
            Self::Conflicted => ProjectAction::ReviewConflicts,
        }
    }

    pub const fn needs_attention(self) -> bool {
        matches!(self, Self::OutOfSync(_) | Self::Conflicted)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectAction {
    Download,
    Open,
    Push,
    Pull,
    ReviewConflicts,
    None,
}

impl ProjectAction {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Download => "↓  DOWNLOAD",
            Self::Open => "OPEN PROJECT",
            Self::Push => "↑  BACK UP CHANGES",
            Self::Pull => "↓  DOWNLOAD LATEST",
            Self::ReviewConflicts => "CHOOSE WHAT TO KEEP",
            Self::None => "SYNCING…",
        }
    }

    pub const fn starts_transfer(self) -> bool {
        matches!(self, Self::Download | Self::Push | Self::Pull)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectVersion {
    pub version: &'static str,
    pub summary: &'static str,
    pub created_at: &'static str,
}

pub struct Project {
    pub id: &'static str,
    pub name: &'static str,
    pub file_name: &'static str,
    pub local_path: &'static str,
    pub size: &'static str,
    pub format: ProjectFormat,
    pub status: ProjectStatus,
    pub last_activity: String,
    pub safe_version: &'static str,
    pub local_inventory: String,
    pub versions: &'static [ProjectVersion],
    /// What the project is, read from its latest commit's summary. `None`
    /// until that summary has been fetched, or for a format Auru does not
    /// read detail from.
    pub detail: Option<ProjectDetail>,
    /// Plugins this computer does not have. Empty is the good case.
    pub missing_plugins: Vec<MissingPlugin>,
}

impl Project {
    #[allow(clippy::too_many_arguments)]
    fn stub(
        id: &'static str,
        name: &'static str,
        file_name: &'static str,
        local_path: &'static str,
        size: &'static str,
        format: ProjectFormat,
        status: ProjectStatus,
        last_activity: &'static str,
        safe_version: &'static str,
        local_inventory: &'static str,
        versions: &'static [ProjectVersion],
    ) -> Self {
        Self {
            id,
            name,
            file_name,
            local_path,
            size,
            format,
            status,
            last_activity: last_activity.to_owned(),
            safe_version,
            local_inventory: local_inventory.to_owned(),
            versions,
            detail: None,
            missing_plugins: Vec::new(),
        }
    }

    /// Attach the detail read from the project's latest commit.
    fn with_detail(mut self, detail: ProjectDetail, missing: Vec<MissingPlugin>) -> Self {
        self.detail = Some(detail);
        self.missing_plugins = missing;
        self
    }

    /// Populate the detail page by actually reading a Live Set.
    ///
    /// The demo projects go through the same path a real one will: normalize
    /// the set, derive its summary, resolve its plugins against the registry.
    /// Hand-writing the numbers instead would let the display drift away from
    /// what the reader actually produces without anything noticing.
    ///
    /// Silently leaves the detail empty if the set cannot be read — a demo
    /// fixture is never worth failing startup over.
    fn with_demo_live_set(self, xml: &str) -> Self {
        let Ok(snapshot) =
            ProjectSnapshot::from_source_bytes(ProjectFormat::AbletonLiveSet, xml.as_bytes())
        else {
            return self;
        };
        let Some(detail) = ProjectInfo::from_snapshot_bytes(snapshot.as_bytes())
            .as_ref()
            .and_then(ProjectDetail::from_project_info)
        else {
            return self;
        };

        // No search paths: the demo is a machine that has none of these
        // installed, which is the state worth showing.
        let missing = ableton::read_plugins(&snapshot)
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
            .unwrap_or_default();

        self.with_detail(detail, missing)
    }

    pub const fn format_label(&self) -> &'static str {
        match self.format {
            ProjectFormat::Dawproject => "DAWPROJECT",
            ProjectFormat::AbletonLiveSet => "ABLETON LIVE SET",
            ProjectFormat::Auru => "AURU PROJECT",
        }
    }

    pub const fn open_label(&self) -> &'static str {
        match self.format {
            ProjectFormat::AbletonLiveSet => "OPEN IN ABLETON LIVE  ⌘↵",
            ProjectFormat::Dawproject => "OPEN IN YOUR DAW  ⌘↵",
            ProjectFormat::Auru => "OPEN IN AURU STUDIO  ⌘↵",
        }
    }

    pub fn list_status(&self) -> String {
        match self.status {
            ProjectStatus::NotDownloaded => "on Auru Cloud only".to_owned(),
            ProjectStatus::Downloaded => self.last_activity.clone(),
            ProjectStatus::Syncing => "syncing · 64%".to_owned(),
            ProjectStatus::OutOfSync(SyncDirection::LocalAhead) => {
                format!("changes waiting · {}", self.last_activity)
            }
            ProjectStatus::OutOfSync(SyncDirection::UpstreamAhead) => {
                "newer on studio mac".to_owned()
            }
            ProjectStatus::Conflicted => "conflict · needs you".to_owned(),
        }
    }

    pub const fn status_headline(&self) -> &'static str {
        match self.status {
            ProjectStatus::NotDownloaded => {
                "A safe copy lives on Auru Cloud — it isn't on this computer."
            }
            ProjectStatus::Downloaded => "Backed up and verified. Safe to unplug.",
            ProjectStatus::Syncing => "Copying and verifying this project now.",
            ProjectStatus::OutOfSync(SyncDirection::LocalAhead) => {
                "Changes on this computer aren't backed up."
            }
            ProjectStatus::OutOfSync(SyncDirection::UpstreamAhead) => {
                "A newer version was saved from Studio Mac."
            }
            ProjectStatus::Conflicted => "This computer and Studio Mac have different edits.",
        }
    }

    pub fn status_explanation(&self) -> String {
        match self.status {
            ProjectStatus::NotDownloaded => {
                format!("Download it to work here ({}).", self.size)
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
        self.last_activity = "started just now".to_owned();
        true
    }

    pub fn finish_transfer(&mut self) {
        if self.status == ProjectStatus::Syncing {
            self.status = ProjectStatus::Downloaded;
            self.last_activity = "backed up · just now".to_owned();
            self.local_inventory = "all files · just downloaded".to_owned();
        }
    }

    pub const fn displayed_path(&self) -> &'static str {
        match self.status {
            ProjectStatus::NotDownloaded => "Not on this computer",
            _ => self.local_path,
        }
    }
}

const MIDNIGHT_VERSIONS: &[ProjectVersion] = &[
    ProjectVersion {
        version: "v41",
        summary: "Rebuilt the bridge, new vocal chain",
        created_at: "yesterday 23:10",
    },
    ProjectVersion {
        version: "v40",
        summary: "Trimmed intro, tempo to 122",
        created_at: "tue 21:04",
    },
    ProjectVersion {
        version: "v39",
        summary: "Hotel demo bounce, rough mix",
        created_at: "mon 09:37",
    },
];

const HOTEL_VERSIONS: &[ProjectVersion] = &[
    ProjectVersion {
        version: "v19",
        summary: "Backed up from this laptop",
        created_at: "just now",
    },
    ProjectVersion {
        version: "v18",
        summary: "Live-take comps from soundcheck",
        created_at: "today 17:55",
    },
    ProjectVersion {
        version: "v17",
        summary: "Muted the brass, wider pads",
        created_at: "today 12:20",
    },
];

const VANTABLACK_VERSIONS: &[ProjectVersion] = &[
    ProjectVersion {
        version: "v33",
        summary: "Master bus limiter settings",
        created_at: "today 18:30",
    },
    ProjectVersion {
        version: "v32",
        summary: "Alt ending — half-time outro",
        created_at: "today 15:02",
    },
    ProjectVersion {
        version: "v31",
        summary: "Bass re-amp from studio",
        created_at: "yesterday 19:44",
    },
];

const TOKYO_VERSIONS: &[ProjectVersion] = &[
    ProjectVersion {
        version: "v26",
        summary: "Sax overdubs and clean-up",
        created_at: "today 17:40",
    },
    ProjectVersion {
        version: "v25",
        summary: "Airport sketch, gate B44",
        created_at: "yesterday 06:15",
    },
    ProjectVersion {
        version: "v24",
        summary: "Bounced stems for Mara",
        created_at: "fri 23:58",
    },
];

const ANALOG_VERSIONS: &[ProjectVersion] = &[
    ProjectVersion {
        version: "v27",
        summary: "Organ takes, first tuning pass",
        created_at: "today 11:08",
    },
    ProjectVersion {
        version: "v26",
        summary: "Choir bus routing",
        created_at: "yesterday 22:19",
    },
    ProjectVersion {
        version: "v25",
        summary: "Room mics from chapel session",
        created_at: "wed 14:30",
    },
];

const GLASSHOUSE_VERSIONS: &[ProjectVersion] = &[
    ProjectVersion {
        version: "v12",
        summary: "Festival edit — tighter drop",
        created_at: "fri 20:11",
    },
    ProjectVersion {
        version: "v11",
        summary: "Extended intro for live set",
        created_at: "thu 16:40",
    },
];

pub fn stub_projects() -> Vec<Project> {
    vec![
        Project::stub(
            "midnight-transit",
            "Midnight Transit",
            "midnight-transit.als",
            "~/Music/Ableton/Midnight Transit/midnight-transit.als",
            "1.2 GB",
            ProjectFormat::AbletonLiveSet,
            ProjectStatus::OutOfSync(SyncDirection::LocalAhead),
            "8 min ago",
            "v41 · yesterday 23:10",
            "128 of 128 · plugins listed",
            MIDNIGHT_VERSIONS,
        )
        .with_demo_live_set(DEMO_LIVE_SET),
        Project::stub(
            "hotel-casablanca",
            "Hotel Casablanca",
            "hotel-casablanca.als",
            "~/Music/Ableton/Hotel Casablanca/hotel-casablanca.als",
            "860 MB",
            ProjectFormat::AbletonLiveSet,
            ProjectStatus::Downloaded,
            "backed up · just now",
            "v19 · just now",
            "74 of 74 files",
            HOTEL_VERSIONS,
        )
        // Everything this one needs is on the machine — the good case, and
        // the one that proves the plugins section stays out of the way.
        .with_detail(
            ProjectDetail {
                tempo: Some(122.0),
                time_signature: Some((4, 4)),
                key: Some("F Minor".to_owned()),
                in_key: false,
                tracks_total: 22,
                tracks_midi: 12,
                tracks_audio: 8,
                tracks_return: 2,
                clip_count: 137,
                bars: Some(164.0),
                live_version: Some("Ableton Live 12.1.1".to_owned()),
                files_total: 74,
                files_gathered: 0,
            },
            Vec::new(),
        ),
        Project::stub(
            "vantablack",
            "Vantablack",
            "vantablack.dawproject",
            "~/Music/Projects/Vantablack/vantablack.dawproject",
            "2.1 GB",
            ProjectFormat::Dawproject,
            ProjectStatus::Syncing,
            "started just now",
            "v33 · today 18:30",
            "203 of 203 · plugins listed",
            VANTABLACK_VERSIONS,
        ),
        Project::stub(
            "tokyo-layover",
            "Tokyo Layover",
            "tokyo-layover.als",
            "~/Music/Ableton/Tokyo Layover/tokyo-layover.als",
            "1.6 GB",
            ProjectFormat::AbletonLiveSet,
            ProjectStatus::OutOfSync(SyncDirection::UpstreamAhead),
            "checked 2 min ago",
            "v26 · today 17:40",
            "from yesterday · 141 files",
            TOKYO_VERSIONS,
        ),
        Project::stub(
            "analog-church",
            "Analog Church",
            "analog-church.dawproject",
            "~/Music/Projects/Analog Church/analog-church.dawproject",
            "3.4 GB",
            ProjectFormat::Dawproject,
            ProjectStatus::Conflicted,
            "needs you",
            "v27 · today 11:08",
            "9 files differ from the safe copy",
            ANALOG_VERSIONS,
        ),
        Project::stub(
            "glasshouse-live",
            "Glasshouse (Live Edit)",
            "glasshouse-live.als",
            "~/Music/Ableton/Glasshouse/glasshouse-live.als",
            "2.4 GB",
            ProjectFormat::AbletonLiveSet,
            ProjectStatus::NotDownloaded,
            "updated fri 20:11",
            "v12 · fri 20:11",
            "not on this computer",
            GLASSHOUSE_VERSIONS,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_ahead_should_offer_backup() {
        let status = ProjectStatus::OutOfSync(SyncDirection::LocalAhead);

        assert_eq!(status.action(), ProjectAction::Push);
        assert!(status.needs_attention());
    }

    #[test]
    fn upstream_ahead_should_offer_download() {
        let status = ProjectStatus::OutOfSync(SyncDirection::UpstreamAhead);

        assert_eq!(status.action(), ProjectAction::Pull);
        assert!(status.needs_attention());
    }

    #[test]
    fn transfer_should_finish_downloaded() {
        let mut project = stub_projects().remove(5);
        assert!(project.begin_transfer());

        project.finish_transfer();

        assert_eq!(project.status, ProjectStatus::Downloaded);
        assert_eq!(project.list_status(), "backed up · just now");
    }

    #[test]
    fn downloading_should_reveal_managed_local_path() {
        let mut project = stub_projects().remove(5);

        assert!(project.begin_transfer());

        assert_eq!(
            project.displayed_path(),
            "~/Music/Ableton/Glasshouse/glasshouse-live.als"
        );
    }

    #[test]
    fn downloaded_project_should_not_need_attention() {
        assert!(!ProjectStatus::Downloaded.needs_attention());
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
    fn a_project_with_everything_present_should_list_no_plugins() {
        // The good case has to stay silent — an empty section would imply
        // there is something to deal with.
        let project = stub_projects().remove(1);
        assert!(project.detail.is_some());
        assert!(project.missing_plugins.is_empty());
    }

    #[test]
    fn the_demo_project_detail_should_come_from_the_ableton_reader() {
        // Not hand-written numbers: this is what the reader produced from
        // DEMO_LIVE_SET, so the display cannot drift from the extraction.
        let project = stub_projects().remove(0);
        let detail = project.detail.expect("read from the demo set");

        assert_eq!(detail.tempo_line(), "175 BPM · 4/4");
        assert_eq!(detail.key_line(), "C Phrygian · in key");
        assert_eq!(
            detail.tracks_line(),
            "6 tracks · 2 MIDI · 2 audio · 2 returns"
        );
        assert_eq!(detail.made_with(), "Ableton Live 12.0.25");
        // 352 beats at 4/4 is 88 bars.
        assert_eq!(detail.length_line(), "88 bars · 4 clips");
    }

    #[test]
    fn missing_plugins_should_say_what_they_are_and_where_to_get_them() {
        let project = stub_projects().remove(0);
        // Looked up by name rather than position: resolution orders by
        // format, which is a presentation detail this test should not pin.
        let serum = project
            .missing_plugins
            .iter()
            .find(|plugin| plugin.name == "Serum 2");

        // On a machine that happens to have Serum installed there is nothing
        // to report, and that is the correct outcome rather than a failure.
        if let Some(serum) = serum {
            assert_eq!(serum.detail_line(), "Xfer Records · VST3 · used 2 times");
            assert_eq!(
                serum.link.as_deref(),
                Some("https://xferrecords.com/products/serum"),
                "a missing plugin should point at where its maker distributes it"
            );
        }

        // Live's own devices are never something to go and get.
        assert!(
            !project
                .missing_plugins
                .iter()
                .any(|plugin| plugin.name == "EQ Eight" || plugin.name == "Reverb"),
            "devices that come with Live must not be listed as missing"
        );
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
        assert!(MissingPlugin::from_resolved(&make(PluginAvailability::BundledWithLive)).is_none());
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

    #[test]
    fn a_format_without_readable_detail_should_yield_none() {
        let info = ProjectInfo {
            schema: auru_pm::PROJECT_INFO_SCHEMA,
            format: ProjectFormat::Dawproject,
            ableton: None,
        };
        assert!(ProjectDetail::from_project_info(&info).is_none());
    }
}

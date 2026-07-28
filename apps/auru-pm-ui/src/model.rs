use auru_pm::ProjectFormat;

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
        }
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
        ),
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
}

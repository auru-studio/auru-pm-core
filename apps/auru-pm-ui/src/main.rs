mod automatic_backup;
mod backend;
mod catalog;
mod menus;
mod model;
mod runtime;
mod state;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use auru_pm::{
    AURU_REGISTRY_URL, AuthMethod, BundlePolicy, ConflictChoice, OAuthProgress, PathAlias,
    RetentionRule,
};
use gpui::{
    Anchor, AnyElement, App, Bounds, Context, Div, ElementId, Entity, FocusHandle, FontWeight,
    Hsla, InteractiveElement, Interactivity, IntoElement, ParentElement, Render, RenderOnce,
    SharedString, Size, Stateful, StyleRefinement, Styled, Subscription, UniformListScrollHandle,
    Window, WindowBounds, WindowOptions, div, prelude::*, px, relative, rgb, uniform_list,
};
use gpui_component::{
    Root, Selectable, Sizable, Theme, ThemeMode, ThemeTokens, WindowExt,
    input::{Input, InputEvent, InputState},
    menu::DropdownMenu,
    notification::Notification,
    scroll::ScrollableElement,
    setting::{SettingField, SettingGroup, SettingItem, SettingPage, Settings},
    spinner::Spinner,
};
use gpui_platform::application;

use crate::automatic_backup::{
    AutomaticBackupReason, AutomaticBackupScheduler, ProjectObservation,
};
use crate::catalog::{
    AuthHint, CatalogState, ProviderAvailability, ProviderListing, fetch_first_party_catalog,
    load_provider_file, local_provider,
};
use crate::menus::{
    AddAbletonProject, AddAuruProject, AddDawproject, AddFlStudioProject, CloseWindow, Minimize,
    OpenSettings, SortByAttentionRequired, SortByLastModifiedLocal, SortByLastModifiedRemote,
    SortByName, SortByRecentlyAdded, Zoom,
};
use crate::model::{
    ImportKind, PLUGIN_SETTINGS_REASSURANCE, Project, ProjectAction, ProjectStatus, SortOrder,
    SyncDirection, WatchedFolder, format_bytes, import_project, load_library,
    replace_provider_projects, sort_projects,
};

const OAUTH_POLL_INTERVAL: Duration = Duration::from_millis(100);
const AUTOMATIC_BACKUP_POLL_INTERVAL: Duration = Duration::from_secs(5);
const DISPLAY_FONT: &str = "New York";
const MONO_FONT: &str = "SF Mono";
const SIDEBAR_WIDTH: f32 = 320.0;
const WAVEFORM_HEIGHTS: [f32; 30] = [
    10.0, 20.0, 14.0, 24.0, 18.0, 12.0, 22.0, 16.0, 25.0, 19.0, 12.0, 16.0, 23.0, 14.0, 18.0, 26.0,
    13.0, 20.0, 17.0, 24.0, 11.0, 15.0, 22.0, 18.0, 25.0, 14.0, 19.0, 12.0, 21.0, 16.0,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Route {
    Library,
    Onboarding,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OnboardingStep {
    Profile,
    Provider,
    Music,
}

impl OnboardingStep {
    const fn position(self) -> (usize, usize) {
        (
            match self {
                Self::Profile => 1,
                Self::Provider => 2,
                Self::Music => 3,
            },
            3,
        )
    }

    const fn next(self) -> Option<Self> {
        match self {
            Self::Profile => Some(Self::Provider),
            Self::Provider => Some(Self::Music),
            Self::Music => None,
        }
    }

    const fn previous(self) -> Option<Self> {
        match self {
            Self::Profile => None,
            Self::Provider => Some(Self::Profile),
            Self::Music => Some(Self::Provider),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Overlay {
    None,
    ProviderPicker,
    Authenticate { provider_index: usize },
    ConflictResolver,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OverlayHost {
    Main,
    Settings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OverlayState {
    host: OverlayHost,
    overlay: Overlay,
}

impl Default for OverlayState {
    fn default() -> Self {
        Self {
            host: OverlayHost::Main,
            overlay: Overlay::None,
        }
    }
}

impl OverlayState {
    fn show(&mut self, host: OverlayHost, overlay: Overlay) {
        debug_assert_ne!(overlay, Overlay::None);
        self.host = host;
        self.overlay = overlay;
    }

    fn replace(&mut self, overlay: Overlay) {
        self.overlay = overlay;
    }

    fn clear(&mut self) {
        self.overlay = Overlay::None;
    }

    fn visible_for(self, host: OverlayHost) -> Option<Overlay> {
        (self.host == host && self.overlay != Overlay::None).then_some(self.overlay)
    }
}

struct PendingConflict {
    project_id: String,
    project_name: String,
    backup: Box<backend::ConflictBackup>,
    choices: Vec<ConflictChoice>,
}

#[derive(Clone, Copy, Debug)]
enum BackupStart {
    Immediate,
    AfterQuietPeriod { qualified_revision: SystemTime },
}

impl BackupStart {
    fn accepts_prepared_revision(self, prepared_revision: Option<SystemTime>) -> bool {
        match self {
            Self::Immediate => true,
            Self::AfterQuietPeriod { qualified_revision } => {
                prepared_revision == Some(qualified_revision)
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum AuthPhase {
    Ready,
    Waiting,
    DeviceCode {
        user_code: String,
        verification_uri: String,
    },
    Complete {
        detail: String,
    },
    Failed(String),
}

/// How much version history to keep after each successful backup.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum VersionRetention {
    #[default]
    Everything,
    LastYear,
    LastFifty,
}

impl VersionRetention {
    const ALL: [Self; 3] = [Self::Everything, Self::LastYear, Self::LastFifty];

    /// Stable key used as the dropdown's value.
    const fn key(self) -> &'static str {
        match self {
            Self::Everything => "everything",
            Self::LastYear => "last-year",
            Self::LastFifty => "last-fifty",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Everything => "Keep every version",
            Self::LastYear => "Keep the last year",
            Self::LastFifty => "Keep the last 50 versions",
        }
    }

    fn from_key(key: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|option| option.key() == key)
            .unwrap_or_default()
    }

    fn rule_at(self, now: i64) -> Option<RetentionRule> {
        const YEAR_SECONDS: i64 = 365 * 24 * 60 * 60;
        match self {
            Self::Everything => None,
            Self::LastYear => Some(RetentionRule::Since {
                timestamp: now.saturating_sub(YEAR_SECONDS),
            }),
            Self::LastFifty => Some(RetentionRule::Latest { count: 50 }),
        }
    }
}

struct ProjectManager {
    focus_handle: FocusHandle,
    projects: Vec<Project>,
    selected_project: usize,
    /// Scroll position of the sidebar list, shared with its scrollbar.
    ///
    /// Held on the manager rather than rebuilt per render: a scroll handle is
    /// the list's position, so a fresh one each frame would snap it to the top.
    list_scroll: UniformListScrollHandle,
    route: Route,
    onboarding_step: OnboardingStep,
    overlay: OverlayState,
    display_name: String,
    display_name_input: Entity<InputState>,
    search_input: Entity<InputState>,
    credential_input: Entity<InputState>,
    path_alias_input: Entity<InputState>,
    providers: Vec<ProviderListing>,
    catalog_state: CatalogState,
    auth_phase: AuthPhase,
    automatic_backups: bool,
    automatic_backup_scheduler: AutomaticBackupScheduler,
    verify_uploads: bool,
    /// How much history providers keep after successful backups.
    version_retention: VersionRetention,
    appearance: Appearance,
    /// What Auru remembers between launches.
    state: crate::state::AppState,
    /// The settings window, while it is open.
    settings_window: Option<gpui::WindowHandle<Root>>,
    /// Editable copy of the display name, shown in Settings.
    display_name_setting: Entity<InputState>,
    /// Folders being watched for projects, and what each one holds.
    watched_folders: Vec<WatchedFolder>,
    /// True while a folder is being scanned.
    scanning: bool,
    /// True while connected provider catalogues are being refreshed.
    remote_refreshing: bool,
    /// A provider changed while a refresh was running; run once more after it.
    remote_refresh_pending: bool,
    /// Provider catalogue failures from the latest refresh.
    remote_discovery_errors: Vec<String>,
    pending_conflict: Option<PendingConflict>,
    _subscriptions: Vec<Subscription>,
}

impl ProjectManager {
    fn new(options: Options, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);

        // The library is a view of what is on disk, so it is built from the
        // folders the person told us about rather than carried in the state
        // file. Only the folders themselves persist.
        let mut state = crate::state::AppState::load();
        let projects = load_library(&mut state);

        // The name someone chose last time, if they have been here before.
        let display_name_seed = state.display_name.clone();
        let initial_route = if !state.onboarding_complete || display_name_seed.trim().is_empty() {
            Route::Onboarding
        } else {
            Route::Library
        };

        let display_name_input = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("e.g. Alice, Bob, or Charlie");
            input.set_value(display_name_seed.as_str(), window, cx);
            input
        });
        // A separate input from onboarding's: Settings edits a name that
        // already exists, so it starts populated rather than empty.
        let display_name_setting = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("Your name");
            state.set_value(display_name_seed.as_str(), window, cx);
            state
        });
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("⌕ search projects…"));
        let credential_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Personal access token"));
        let path_alias_input = cx
            .new(|cx| InputState::new(window, cx).placeholder("Recorded prefix, e.g. D:\\Samples"));

        let _subscriptions = [
            &display_name_input,
            &display_name_setting,
            &search_input,
            &credential_input,
            &path_alias_input,
        ]
        .into_iter()
        .map(|input| {
            cx.subscribe_in(input, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            })
        })
        .collect();

        // A providers file replaces the hosted registry outright: it was
        // passed deliberately, so silently going to the network instead would
        // be the wrong answer. Loading it is a small synchronous file read.
        let file_providers = options
            .providers_file
            .as_deref()
            .map(|path| (path.to_path_buf(), load_provider_file(path)));

        match &file_providers {
            Some((_, Ok(_))) | None => {}
            Some((path, Err(message))) => {
                eprintln!(
                    "[auru-pm] ignoring --providers-file {}: {message}",
                    path.display()
                );
            }
        }
        let file_providers = file_providers.and_then(|(_, result)| result.ok());

        if file_providers.is_none() {
            let catalog_task = cx
                .background_executor()
                .spawn(async { fetch_first_party_catalog() });
            cx.spawn(async move |this, cx| {
                let catalog = catalog_task.await;
                _ = this.update(cx, |this, cx| {
                    match catalog {
                        Ok(mut providers) => {
                            for provider in &mut providers {
                                if this.state.is_provider_connected(&provider.entry.id)
                                    || this.providers.iter().any(|current| {
                                        current.entry.id == provider.entry.id
                                            && current.availability
                                                == ProviderAvailability::Connected
                                    })
                                {
                                    provider.mark_connected();
                                }
                            }
                            // The published list does not know about the
                            // user's own folders, so carry those across rather
                            // than replacing the lot — otherwise a NAS added
                            // before the fetch returned would vanish.
                            providers.extend(
                                this.providers
                                    .iter()
                                    .filter(|current| current.is_local())
                                    .cloned(),
                            );
                            this.providers = providers;
                            this.catalog_state = CatalogState::Live;
                            this.refresh_remote_projects(cx);
                        }
                        Err(_) => this.catalog_state = CatalogState::Unreachable,
                    }
                    cx.notify();
                });
            })
            .detach();
        }

        // Local destinations are the user's own and are known immediately;
        // the published list arrives later, or not at all.
        let local: Vec<ProviderListing> = state
            .local_providers
            .iter()
            .map(|path| {
                let mut provider = local_provider(path);
                if state.is_provider_connected(&provider.entry.id) {
                    provider.mark_connected();
                }
                provider
            })
            .collect();

        let (mut providers, catalog_state) = match file_providers {
            Some(mut providers) => {
                providers.extend(local);
                (providers, CatalogState::FromFile)
            }
            // Deliberately no placeholder entries. An invented provider list is
            // worse than an empty one: every row is something the person cannot
            // actually connect to, and they have no way to tell which.
            None => (local, CatalogState::Loading),
        };
        for provider in &mut providers {
            if state.is_provider_connected(&provider.entry.id) {
                provider.mark_connected();
            }
        }
        let automatic_backup_scheduler =
            AutomaticBackupScheduler::from_attempted_revisions(state.backup_attempts());

        Self {
            focus_handle,
            projects,
            selected_project: 0,
            list_scroll: UniformListScrollHandle::default(),
            route: initial_route,
            onboarding_step: OnboardingStep::Profile,
            overlay: OverlayState::default(),
            display_name: state.display_name.clone(),
            display_name_input,
            search_input,
            credential_input,
            path_alias_input,
            providers,
            catalog_state,
            auth_phase: AuthPhase::Ready,
            automatic_backups: state.automatic_backups,
            automatic_backup_scheduler,
            verify_uploads: state.verify_uploads,
            version_retention: VersionRetention::from_key(&state.version_retention),
            appearance: Appearance::from_key(&state.appearance),
            state,
            settings_window: None,
            display_name_setting,
            watched_folders: Vec::new(),
            scanning: false,
            remote_refreshing: false,
            remote_refresh_pending: false,
            remote_discovery_errors: Vec::new(),
            pending_conflict: None,
            _subscriptions,
        }
    }

    fn close_window(&mut self, _: &CloseWindow, window: &mut Window, _: &mut Context<Self>) {
        window.remove_window();
    }

    fn minimize_window(&mut self, _: &Minimize, window: &mut Window, _: &mut Context<Self>) {
        window.minimize_window();
    }

    fn zoom_window(&mut self, _: &Zoom, window: &mut Window, _: &mut Context<Self>) {
        window.zoom_window();
    }

    fn add_ableton_project(
        &mut self,
        _: &AddAbletonProject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_project(ImportKind::AbletonLiveSet, window, cx);
    }

    fn sort_by_last_modified_local(
        &mut self,
        _: &SortByLastModifiedLocal,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_sort_order(SortOrder::LastModifiedLocal, cx);
    }

    fn sort_by_last_modified_remote(
        &mut self,
        _: &SortByLastModifiedRemote,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_sort_order(SortOrder::LastModifiedRemote, cx);
    }

    fn sort_by_name(&mut self, _: &SortByName, _: &mut Window, cx: &mut Context<Self>) {
        self.set_sort_order(SortOrder::NameAscending, cx);
    }

    fn sort_by_recently_added(
        &mut self,
        _: &SortByRecentlyAdded,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_sort_order(SortOrder::RecentlyAdded, cx);
    }

    fn sort_by_attention_required(
        &mut self,
        _: &SortByAttentionRequired,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_sort_order(SortOrder::AttentionRequired, cx);
    }

    /// The order the sidebar is currently in.
    fn sort_order(&self) -> SortOrder {
        SortOrder::from_key(&self.state.sort_order)
    }

    /// Re-order the library, and remember the choice for next launch.
    fn set_sort_order(&mut self, order: SortOrder, cx: &mut Context<Self>) {
        if self.sort_order() == order {
            return;
        }
        self.state.sort_order = order.key().to_owned();
        self.state.save();

        // Selection is an index into a list that is about to move, so hold on
        // to what is actually selected and find it again afterwards. Without
        // this, re-sorting silently swaps the open project for whichever one
        // happens to land in the same slot.
        let selected = self
            .projects
            .get(self.selected_project)
            .map(|project| project.id.clone());
        sort_projects(&mut self.projects, order);
        self.selected_project = selected
            .and_then(|id| self.projects.iter().position(|project| project.id == id))
            .unwrap_or(0);
        cx.notify();
    }

    fn add_flstudio_project(
        &mut self,
        _: &AddFlStudioProject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_project(ImportKind::FlStudio, window, cx);
    }

    fn add_dawproject(&mut self, _: &AddDawproject, window: &mut Window, cx: &mut Context<Self>) {
        self.add_project(ImportKind::Dawproject, window, cx);
    }

    fn add_auru_project(
        &mut self,
        _: &AddAuruProject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_project(ImportKind::AuruProject, window, cx);
    }

    /// The folders being watched, and what each was found to hold.
    fn render_watched_folders(&self, cx: &mut Context<Self>) -> AnyElement {
        let total_projects: usize = self
            .watched_folders
            .iter()
            .map(WatchedFolder::project_count)
            .sum();
        let total_bytes: u64 = self
            .watched_folders
            .iter()
            .map(WatchedFolder::total_bytes)
            .sum();

        let mut section = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(section_label("[ WHERE YOUR MUSIC LIVES ]"));

        if self.watched_folders.is_empty() {
            section = section.child(
                div()
                    .border_1()
                    .border_color(line())
                    .p_4()
                    .text_size(px(9.0))
                    .text_color(dim())
                    .child(
                        "No folders watched yet. Point Auru at where you keep your projects \
                         and it will find them.",
                    ),
            );
        }

        section = section.children(self.watched_folders.iter().map(|folder| {
            let count = folder.project_count();
            div()
                .flex()
                .flex_col()
                .gap_2()
                .border_1()
                .border_color(line())
                .p_4()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_4()
                        .child(
                            div()
                                .min_w_0()
                                .overflow_x_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_size(px(10.0))
                                .text_color(bright())
                                .child(folder.display_path().to_uppercase()),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(8.0))
                                .text_color(if count == 0 { faint() } else { green() })
                                .child(format!(
                                    "{count} PROJECT{} · {}",
                                    if count == 1 { "" } else { "S" },
                                    format_bytes(folder.total_bytes())
                                )),
                        ),
                )
                // A few names, so the number is recognisable as their music
                // rather than an abstract count.
                .children(folder.projects.iter().take(4).map(|project| {
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_4()
                        .text_size(px(9.0))
                        .child(
                            div()
                                .min_w_0()
                                .overflow_x_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_color(ink())
                                .child(project.name.clone()),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(faint())
                                .child(project.size_label()),
                        )
                        .into_any_element()
                }))
                .children((count > 4).then(|| {
                    div()
                        .text_size(px(9.0))
                        .text_color(faint())
                        .child(format!("… and {} more", count - 4))
                        .into_any_element()
                }))
                .into_any_element()
        }));

        let mut watch_button = div()
            .id("watch-another-folder")
            .flex()
            .h(px(44.0))
            .items_center()
            .border_1()
            .border_color(line())
            .px_4()
            .text_size(px(8.0))
            .text_color(faint());

        if self.scanning {
            watch_button = watch_button
                .gap_2()
                .child(Spinner::new().xsmall().color(blue()))
                .child("SCANNING…");
        } else {
            watch_button = watch_button
                .cursor_pointer()
                .hover(|this| this.border_color(green()).text_color(green()))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.watch_another_folder(window, cx);
                }))
                .child("＋ WATCH ANOTHER FOLDER");
        }
        section = section.child(watch_button);

        // The promise the setup screen makes, repeated where the folders are.
        // Finding a project is not agreeing to upload it, and on a metered
        // provider that difference is somebody's bill.
        if total_projects > 0 {
            section = section.child(div().text_size(px(8.0)).text_color(amber()).child(format!(
                "{total_projects} PROJECTS · {} — NOTHING UPLOADS UNTIL YOU CHOOSE",
                format_bytes(total_bytes)
            )));
        }

        section.into_any_element()
    }

    fn render_path_aliases(&self, cx: &mut Context<Self>) -> AnyElement {
        let aliases = self.state.path_aliases().to_vec();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(section_label("[ PATHS FROM OTHER COMPUTERS ]"))
            .children(aliases.into_iter().enumerate().map(|(index, alias)| {
                let from = alias.from.clone();
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .border_1()
                    .border_color(line())
                    .p_3()
                    .text_size(px(9.0))
                    .child(div().min_w_0().flex_1().text_color(ink()).child(format!(
                        "{}  →  {}",
                        alias.from,
                        alias.to.display()
                    )))
                    .child(
                        div()
                            .id(("remove-path-alias", index))
                            .cursor_pointer()
                            .text_color(faint())
                            .hover(|this| this.text_color(red()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.state.remove_path_alias(&from);
                                this.state.save();
                                cx.notify();
                            }))
                            .child("REMOVE"),
                    )
                    .into_any_element()
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Input::new(&self.path_alias_input).flex_1())
                    .child(
                        div()
                            .id("add-path-alias")
                            .flex()
                            .h(px(36.0))
                            .items_center()
                            .border_1()
                            .border_color(line())
                            .px_3()
                            .cursor_pointer()
                            .text_size(px(8.0))
                            .text_color(faint())
                            .hover(|this| this.border_color(green()).text_color(green()))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.add_path_alias(window, cx);
                            }))
                            .child("CHOOSE LOCAL FOLDER"),
                    ),
            )
            .child(
                div()
                    .text_size(px(8.0))
                    .line_height(relative(1.5))
                    .text_color(dim())
                    .child(
                        "Enter the path prefix stored by the other computer, then choose where \
                         that folder lives here. The mapping is used for Ableton and FL samples.",
                    ),
            )
            .into_any_element()
    }

    fn add_path_alias(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let from = self.path_alias_input.read(cx).value().trim().to_owned();
        if from.is_empty() {
            window.push_notification(
                Notification::warning("Enter the path prefix written by the other computer.")
                    .title("Recorded path needed"),
                cx,
            );
            return;
        }
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Map the recorded path to this folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            _ = this.update_in(cx, |this, window, cx| {
                this.state.set_path_alias(&from, &path);
                this.state.save();
                this.path_alias_input.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                });
                window.push_notification(
                    Notification::success(format!(
                        "Projects referring to {from} will look in {}.",
                        path.display()
                    ))
                    .title("Path mapping saved"),
                    cx,
                );
                cx.notify();
            });
        })
        .detach();
    }

    /// Add a folder as a backup destination.
    ///
    /// A drive or a NAS share with no Auru software on it is a perfectly good
    /// second home for a project. Treating it as a provider that needs no
    /// authentication means pushing, history and restore work against it
    /// unchanged, rather than it being a special case threaded through the app.
    fn add_local_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Use this folder for backups".into()),
        });

        cx.spawn_in(window, async move |this, cx| {
            // A cancelled dialog is an ordinary outcome, not an error.
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };

            _ = this.update_in(cx, |this, window, cx| {
                let mut listing = local_provider(&path);
                listing.mark_connected();
                let provider_id = listing.entry.id.clone();
                let name = listing.entry.name.clone();

                // Adding the same folder twice replaces rather than duplicates:
                // the identity is the path.
                if let Some(existing) = this
                    .providers
                    .iter_mut()
                    .find(|provider| provider.entry.id == listing.entry.id)
                {
                    *existing = listing;
                } else {
                    this.providers.push(listing);
                }

                this.state.add_local_provider(&path);
                this.state.connect_provider(&provider_id);
                this.state.save();
                this.refresh_remote_projects(cx);

                window.push_notification(
                    Notification::success(format!(
                        "{name} is ready to use. Nothing has been uploaded yet."
                    ))
                    .title("Local destination added"),
                    cx,
                );
                cx.notify();
            });
        })
        .detach();
    }

    /// Pick a folder and scan it for projects.
    ///
    /// Scanning is the whole action. Nothing is read into the library, nothing
    /// is uploaded — a person needs to see what a folder holds before deciding
    /// what to do about it, and on a metered provider that decision is theirs
    /// to make deliberately.
    fn watch_another_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Watch this folder".into()),
        });

        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };

            _ = this.update(cx, |this, cx| {
                this.scanning = true;
                cx.notify();
            });

            // A real library ran to 653 projects; walking it takes long
            // enough that doing it on the UI thread would show.
            let scanned = cx
                .background_executor()
                .spawn(async move { WatchedFolder::scan(&path) })
                .await;

            _ = this.update_in(cx, |this, window, cx| {
                this.scanning = false;
                this.record_watched_folder(scanned, window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Record a scanned folder, replacing any earlier scan of the same path.
    fn record_watched_folder(
        &mut self,
        folder: WatchedFolder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let found = folder.project_count();
        let where_ = folder.display_path();
        let size = format_bytes(folder.total_bytes());

        self.state.watch(&folder.path);
        self.state.save();
        match self
            .watched_folders
            .iter()
            .position(|existing| existing.path == folder.path)
        {
            Some(index) => self.watched_folders[index] = folder,
            None => self.watched_folders.push(folder),
        }
        // The folder's projects join the library straight away — finding them
        // is the point, and nothing about listing them uploads anything.
        self.projects = load_library(&mut self.state);

        let notification = if found == 0 {
            Notification::warning(format!(
                "No Ableton projects in {where_}. Auru looks for folders Live has marked as projects."
            ))
            .title("Nothing found there")
        } else {
            // Say plainly that finding is not uploading. This is the promise
            // the setup screen makes, and it has to hold everywhere.
            Notification::success(format!(
                "{found} project{} · {size}. Nothing has been uploaded — you choose what gets backed up.",
                if found == 1 { "" } else { "s" }
            ))
            .title(format!("Watching {where_}"))
        };
        window.push_notification(notification, cx);
    }

    /// Ask for a project of `kind` and add it to the library.
    ///
    /// Reading a project means gunzipping and parsing megabytes of XML, so it
    /// happens off the UI thread; the window stays responsive while a large
    /// set is read.
    fn add_project(&mut self, kind: ImportKind, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: kind.accepts_directories(),
            multiple: false,
            prompt: Some(kind.prompt().into()),
        });

        cx.spawn_in(window, async move |this, cx| {
            // A cancelled dialog is an ordinary outcome, not an error.
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };

            let imported = cx
                .background_executor()
                .spawn(async move { import_project(kind, &path) })
                .await;

            _ = this.update_in(cx, |this, window, cx| {
                match imported {
                    Ok(project) => this.insert_project(project, window, cx),
                    Err(message) => window.push_notification(
                        Notification::error(message).title("Couldn't add that project"),
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Add an imported project, replacing any earlier import of the same one.
    ///
    /// Adding the same project twice is a refresh, not a duplicate — the id is
    /// derived from where the project lives, so the second add updates what is
    /// already there.
    fn insert_project(&mut self, project: Project, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(root) = project.live_set.as_ref().and_then(|set| set.parent()) {
            self.state.add_project(root);
            self.state.save();
        }
        let name = project.name.clone();
        let missing = project.missing_plugins.len();
        let replaced = self
            .projects
            .iter()
            .position(|existing| existing.id == project.id);

        let index = match replaced {
            Some(index) => {
                self.projects[index] = project;
                index
            }
            None => {
                self.projects.push(project);
                self.projects.len() - 1
            }
        };
        self.selected_project = index;
        self.route = Route::Library;

        // Say what was found rather than just that something happened — the
        // plugin count is the thing worth knowing before opening it.
        let detail = match missing {
            0 => "Read and ready to back up.".to_owned(),
            1 => "Read. One plugin isn't on this computer.".to_owned(),
            count => format!("Read. {count} plugins aren't on this computer."),
        };
        let title = if replaced.is_some() {
            format!("{name} refreshed")
        } else {
            format!("{name} added")
        };
        window.push_notification(Notification::success(detail).title(title), cx);
    }

    fn open_settings_action(&mut self, _: &OpenSettings, _: &mut Window, cx: &mut Context<Self>) {
        self.route = Route::Library;
        self.open_settings(cx);
    }

    fn handle_project_action(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.projects.get(index) else {
            return;
        };
        let action = project.status.action();
        let project_name = project.name.clone();

        match action {
            ProjectAction::Push => {
                self.start_project_backup(index, BackupStart::Immediate, window, cx);
            }
            ProjectAction::Download => {
                let Some(commit_id) = project.remote.as_ref().map(|remote| remote.head) else {
                    window.push_notification(
                        Notification::error("The provider did not identify a version to restore.")
                            .title("Can't download this project"),
                        cx,
                    );
                    return;
                };
                self.restore_version(index, commit_id, window, cx);
            }
            ProjectAction::Pull => {
                let Some(commit_id) = project.versions.first().map(|version| version.id) else {
                    window.push_notification(
                        Notification::warning("Refresh this project's history and try again.")
                            .title("Latest version is not loaded yet"),
                        cx,
                    );
                    return;
                };
                self.restore_version(index, commit_id, window, cx);
            }
            ProjectAction::Open => {
                let Some(path) = project.live_set.as_deref() else {
                    return;
                };
                if let Err(message) = backend::open_project(path) {
                    window.push_notification(
                        Notification::error(message).title(format!("Couldn't open {project_name}")),
                        cx,
                    );
                }
            }
            ProjectAction::ReviewConflicts => {
                if self
                    .pending_conflict
                    .as_ref()
                    .is_some_and(|pending| pending.project_id == project.id)
                {
                    self.overlay
                        .show(OverlayHost::Main, Overlay::ConflictResolver);
                    cx.notify();
                } else {
                    window.push_notification(
                        Notification::warning(
                            "Run the backup again to refresh the conflicting fields.",
                        )
                        .title(format!("{project_name} needs a decision")),
                        cx,
                    );
                }
            }
            ProjectAction::None => {}
        }
    }

    fn start_project_backup(
        &mut self,
        index: usize,
        start: BackupStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.get(index) else {
            return;
        };
        let Some(project_path) = project.live_set.clone() else {
            window.push_notification(
                Notification::error("The project file is not on this computer.")
                    .title("Can't back up this project"),
                cx,
            );
            return;
        };
        let Some(provider) = self.backup_destination_for(&project_path) else {
            self.overlay
                .show(OverlayHost::Main, Overlay::ProviderPicker);
            window.push_notification(
                Notification::warning("Connect a provider or add a local backup folder first.")
                    .title("Choose where backups live"),
                cx,
            );
            cx.notify();
            return;
        };
        let project_id = project.id.clone();
        let project_name = project.name.clone();
        let display_name = self.display_name.clone();
        let verify_uploads = self.verify_uploads;
        let bundle_policy = self.backup_bundle_policy();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or_default();
        let retention_rule = self.version_retention.rule_at(now);
        let Some(project) = self.projects.get_mut(index) else {
            return;
        };
        if !project.begin_transfer() {
            return;
        }
        // Completion comes only from the real coordinator. The core does not
        // expose byte-level progress yet, so the UI stays indeterminate rather
        // than inventing a percentage.
        cx.notify();

        let preparation = cx
            .background_executor()
            .spawn(async move { backend::prepare_backup(project_path) });
        cx.spawn_in(window, async move |this, cx| {
            let prepared = match preparation.await {
                Ok(prepared) => prepared,
                Err(message) => {
                    _ = this.update_in(cx, |this, window, cx| {
                        if let Some(project) = this
                            .projects
                            .iter_mut()
                            .find(|project| project.id == project_id)
                        {
                            project.fail_transfer();
                        }
                        window.push_notification(
                            Notification::error(message)
                                .title(format!("Couldn't back up {project_name}")),
                            cx,
                        );
                        cx.notify();
                    });
                    return;
                }
            };
            let source_revision = prepared.source_revision();
            if !start.accepts_prepared_revision(source_revision) {
                // A save landed after the scheduler selected this project.
                // Preparation is not publication, so leave the new revision
                // unattempted and let it complete its own quiet period.
                _ = this.update_in(cx, |this, _, cx| {
                    if let Some(project) = this
                        .projects
                        .iter_mut()
                        .find(|project| project.id == project_id)
                    {
                        project.fail_transfer();
                    }
                    cx.notify();
                });
                return;
            }
            let recorded = this
                .update_in(cx, |this, window, cx| {
                    // This durable write must finish before publication. The
                    // prepared snapshot can include a save newer than the
                    // revision observed when the transfer was first queued.
                    if let Err(message) =
                        this.record_backup_revision(&project_id, source_revision)
                    {
                        if let Some(project) = this
                            .projects
                            .iter_mut()
                            .find(|project| project.id == project_id)
                        {
                            project.fail_transfer();
                        }
                        window.push_notification(
                            Notification::error(message)
                                .title(format!("Couldn't back up {project_name}")),
                            cx,
                        );
                        cx.notify();
                        return false;
                    }
                    true
                })
                .unwrap_or(false);
            if !recorded {
                return;
            }
            let backup = cx.background_executor().spawn(async move {
                backend::back_up_prepared(
                    provider,
                    prepared,
                    display_name,
                    retention_rule,
                    verify_uploads,
                    bundle_policy,
                )
            });
            let result = backup.await;
            _ = this.update_in(cx, |this, window, cx| {
                let Some(project) = this
                    .projects
                    .iter_mut()
                    .find(|project| project.id == project_id)
                else {
                    return;
                };
                match result {
                    Ok(backend::BackupResult::Committed(receipt)) => {
                        let (mut detail, verification_failed) = match &receipt.verification {
                            backend::BackupVerification::Skipped => {
                                ("Project bytes and history were stored.".to_owned(), false)
                            }
                            backend::BackupVerification::Verified => (
                                "Project bytes and history were stored and verified.".to_owned(),
                                false,
                            ),
                            backend::BackupVerification::Failed(error) => (
                                format!(
                                    "Project was stored, but verification could not confirm the \
                                     copy: {error}."
                                ),
                                true,
                            ),
                        };
                        if let Some(warning) = receipt.retention_warning {
                            detail.push_str(&format!(" {warning}."));
                        } else if let Some(report) =
                            receipt.retention.filter(|report| report.versions_removed > 0)
                        {
                            if report.bytes_freed > 0 {
                                detail.push_str(&format!(
                                    " Removed {} old version(s) and freed {}.",
                                    report.versions_removed,
                                    format_bytes(report.bytes_freed)
                                ));
                            } else {
                                detail.push_str(&format!(
                                    " Removed {} old version(s).",
                                    report.versions_removed
                                ));
                            }
                        }
                        project.finish_transfer(receipt.history);
                        let notification = if verification_failed {
                            Notification::warning(detail)
                        } else {
                            Notification::success(detail)
                        };
                        window.push_notification(
                            notification.title(format!("{project_name} backed up")),
                            cx,
                        );
                    }
                    Ok(backend::BackupResult::NeedsResolution(conflict)) => {
                        let conflict_count = conflict.conflicts().len();
                        project.status = ProjectStatus::Conflicted;
                        project.sync_progress = 0.0;
                        this.pending_conflict = Some(PendingConflict {
                            project_id: project_id.clone(),
                            project_name: project_name.clone(),
                            choices: vec![ConflictChoice::Local; conflict_count],
                            backup: conflict,
                        });
                        this.overlay
                            .show(OverlayHost::Main, Overlay::ConflictResolver);
                        window.push_notification(
                            Notification::warning(format!(
                                "{conflict_count} conflicting field(s) need a choice before anything is committed."
                            ))
                            .title(format!("{project_name} needs review")),
                            cx,
                        );
                    }
                    Ok(backend::BackupResult::NeedsReview(problems)) => {
                        project.status = ProjectStatus::Conflicted;
                        project.sync_progress = 0.0;
                        window.push_notification(
                            Notification::warning(format!(
                                "The merged project has {problems} integrity problem(s); your local copy is stashed safely."
                            ))
                            .title(format!("{project_name} needs review")),
                            cx,
                        );
                    }
                    Err(message) => {
                        project.fail_transfer();
                        window.push_notification(
                            Notification::error(message)
                                .title(format!("Couldn't back up {project_name}")),
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn record_backup_revision(
        &mut self,
        project_id: &str,
        revision: Option<SystemTime>,
    ) -> Result<(), String> {
        self.automatic_backup_scheduler
            .record_backup_attempt(project_id.to_owned(), revision);
        self.state.record_backup_attempt(project_id, revision);
        self.state
            .save_checked()
            .map_err(|error| format!("Record the project revision before uploading: {error}"))
    }

    fn backup_bundle_policy(&self) -> BundlePolicy {
        let mut policy = BundlePolicy::default();
        for saved in self.state.path_aliases() {
            let alias = PathAlias::new(saved.from.clone(), saved.to.clone());
            match policy
                .path_aliases
                .iter()
                .position(|known| known.from.eq_ignore_ascii_case(&alias.from))
            {
                Some(index) => policy.path_aliases[index] = alias,
                None => policy.path_aliases.push(alias),
            }
        }
        policy
    }

    /// Poll project modification times and enqueue revisions that have been
    /// quiet for the configured automatic-backup window.
    fn start_automatic_backup_watcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |entity, cx| {
            loop {
                cx.background_executor()
                    .timer(AUTOMATIC_BACKUP_POLL_INTERVAL)
                    .await;
                let keep_running = entity
                    .update_in(cx, |manager, window, cx| {
                        manager.poll_automatic_backups(SystemTime::now(), window, cx);
                        true
                    })
                    .unwrap_or(false);
                if !keep_running {
                    break;
                }
            }
        })
        .detach();
    }

    fn poll_automatic_backups(
        &mut self,
        now: SystemTime,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.automatic_backups {
            return;
        }
        for project in &mut self.projects {
            project.refresh_local_file_state();
        }

        let observations: Vec<ProjectObservation> = self
            .projects
            .iter()
            .filter_map(|project| {
                let project_path = project.live_set.as_deref()?;
                Some(ProjectObservation {
                    project_id: project.id.clone(),
                    modified_at: project.modified_at,
                    backed_up_at: project.backed_up_at,
                    backup_destination_ready: project.backed_up_at.is_some()
                        && backend::primary_listing(&self.providers, project_path)
                            .is_some_and(|provider| provider.is_connected()),
                    // Downloaded is deliberately allowed here. A save made
                    // during an upload can have an mtime older than the
                    // sidecar completion time, while the scheduler's captured
                    // revision still proves it needs another backup.
                    backup_blocked: matches!(
                        project.status,
                        ProjectStatus::NotDownloaded
                            | ProjectStatus::Syncing
                            | ProjectStatus::OutOfSync(SyncDirection::UpstreamAhead)
                            | ProjectStatus::Conflicted
                    ),
                })
            })
            .collect();
        let ready = self.automatic_backup_scheduler.poll(now, observations);
        for candidate in ready {
            if let Some(index) = self
                .projects
                .iter()
                .position(|project| project.id == candidate.project_id)
            {
                if candidate.reason == AutomaticBackupReason::SavedDuringPreviousBackup
                    && self.projects[index].status == ProjectStatus::Downloaded
                {
                    self.projects[index].status =
                        ProjectStatus::OutOfSync(SyncDirection::LocalAhead);
                }
                self.start_project_backup(
                    index,
                    BackupStart::AfterQuietPeriod {
                        qualified_revision: candidate.qualified_revision,
                    },
                    window,
                    cx,
                );
            }
        }
    }

    fn back_up_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pending: Vec<usize> = self
            .projects
            .iter()
            .enumerate()
            .filter_map(|(index, project)| {
                (project.status.action() == ProjectAction::Push).then_some(index)
            })
            .collect();
        for index in pending {
            self.start_project_backup(index, BackupStart::Immediate, window, cx);
        }
    }

    /// Open the settings window, or bring it forward if it is already open.
    fn open_settings(&mut self, cx: &mut Context<Self>) {
        let manager = cx.entity();
        // Deferred so the window opens after this event finishes; opening one
        // from inside a click handler on the window that owns the entity would
        // re-enter it.
        cx.defer(move |cx| open_settings_window(manager, cx));
    }

    /// Switch the whole app's appearance.
    ///
    /// Both halves move together: Auru's own palette and gpui-component's
    /// theme. Changing one alone leaves half the window in the old colours,
    /// which is what a partly-applied theme looks like.
    fn apply_appearance(&mut self, appearance: Appearance, cx: &mut Context<Self>) {
        self.appearance = appearance;
        self.state.appearance = appearance.key().to_owned();
        self.state.save();
        appearance.set();
        Theme::change(appearance.theme_mode(), None, cx);
        tint_component_theme(cx);
        cx.notify();
    }

    /// Take the display name from its input, if it says anything.
    ///
    /// An empty box is treated as "no change" rather than as a request to be
    /// nameless — history rows are attributed by this, so a blank one would
    /// make past work look anonymous.
    fn commit_display_name(&mut self, cx: &mut Context<Self>) {
        let typed = self.display_name_setting.read(cx).value().trim().to_owned();
        if typed.is_empty() || typed == self.display_name {
            return;
        }
        self.display_name = typed.clone();
        self.state.display_name = typed;
        self.state.save();
        cx.notify();
    }

    fn advance_onboarding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.onboarding_step {
            OnboardingStep::Profile => {
                let display_name = self.display_name_input.read(cx).value().trim().to_owned();
                if display_name.is_empty() {
                    window.push_notification(
                        Notification::warning("Add the name you want shown on project history.")
                            .title("Display name needed"),
                        cx,
                    );
                    return;
                }
                self.display_name = display_name;
                self.state.display_name = self.display_name.clone();
                self.state.save();
                self.onboarding_step = self
                    .onboarding_step
                    .next()
                    .expect("profile is followed by provider setup");
            }
            OnboardingStep::Provider => {
                self.onboarding_step = self
                    .onboarding_step
                    .next()
                    .expect("provider setup is followed by project folders");
            }
            OnboardingStep::Music => {
                self.state.onboarding_complete = true;
                self.state.save();
                self.route = Route::Library;
                window.push_notification(
                    Notification::success(
                        "Your profile, backup destination, and project folders are ready.",
                    )
                    .title("Welcome to Auru PM"),
                    cx,
                );
            }
        }
        cx.notify();
    }

    fn previous_onboarding_step(&mut self, cx: &mut Context<Self>) {
        if let Some(previous) = self.onboarding_step.previous() {
            self.onboarding_step = previous;
        } else {
            self.route = Route::Library;
        }
        cx.notify();
    }

    fn mark_provider_connected(&mut self, provider_index: usize) -> Option<String> {
        let provider = self.providers.get_mut(provider_index)?;
        provider.mark_connected();
        let provider_id = provider.entry.id.clone();
        let provider_name = provider.entry.name.clone();
        self.state.connect_provider(&provider_id);
        self.state.save();
        Some(provider_name)
    }

    fn backup_destination_for(&self, project_path: &std::path::Path) -> Option<ProviderListing> {
        backend::primary_listing(&self.providers, project_path)
            .or_else(|| {
                self.state.primary_provider.as_ref().and_then(|primary| {
                    self.providers
                        .iter()
                        .find(|provider| provider.entry.id == *primary && provider.is_connected())
                        .cloned()
                })
            })
            .or_else(|| backend::default_listing(&self.providers))
    }

    fn select_provider(
        &mut self,
        provider_index: usize,
        overlay_host: OverlayHost,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(provider) = self.providers.get(provider_index) else {
            return;
        };
        let requires_authentication = provider.requires_authentication();
        let fallback_name = provider.entry.name.clone();

        if requires_authentication {
            self.credential_input
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.auth_phase = AuthPhase::Ready;
            self.overlay.show(
                overlay_host,
                Overlay::Authenticate { provider_index },
            );
        } else {
            let provider_name = self
                .mark_provider_connected(provider_index)
                .unwrap_or(fallback_name);
            self.overlay.clear();
            window.push_notification(
                Notification::success("No sign-in was requested by this provider.")
                    .title(format!("{provider_name} connected")),
                cx,
            );
        }
        cx.notify();
    }

    fn begin_provider_auth(
        &mut self,
        provider_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(provider) = self.providers.get(provider_index) else {
            return;
        };
        let auth_method = provider.preferred_auth_method();
        let provider_id = provider.entry.id.clone();
        let endpoint = provider.entry.endpoint.clone();

        if auth_method == AuthMethod::Pat
            && self.credential_input.read(cx).value().trim().is_empty()
        {
            window.push_notification(
                Notification::warning("Enter the token issued by this provider.")
                    .title("Credential needed"),
                cx,
            );
            return;
        }

        match auth_method {
            AuthMethod::Pat => {
                let token = self.credential_input.read(cx).value().trim().to_owned();
                match auru_pm::token_store::store_provider_token(&provider_id, &token) {
                    Ok(()) => {
                        self.auth_phase = AuthPhase::Complete {
                            detail: "The token is stored securely and will be checked on the first project request."
                                .to_owned(),
                        };
                    }
                    Err(error) => {
                        self.auth_phase =
                            AuthPhase::Failed(format!("Couldn't store the token: {error}"));
                    }
                }
                cx.notify();
            }
            auth_method
            @ (AuthMethod::OAuthAuthorizationCodePkce | AuthMethod::OAuthDeviceCode) => {
                self.auth_phase = AuthPhase::Waiting;
                cx.notify();
                let probe = cx.background_executor().spawn(async move {
                    match runtime::block_on(auru_pm::HttpProvider::probe_health(&endpoint)) {
                        Ok(Ok(health)) => Ok((health, endpoint)),
                        Ok(Err(error)) => Err(error.to_string()),
                        Err(error) => Err(error),
                    }
                });
                cx.spawn(async move |this, cx| {
                    let receiver = match probe.await {
                        Ok((health, endpoint)) => match health.authentication {
                            Some(configuration) => {
                                auru_pm::start_standard_oauth_flow(configuration)
                            }
                            None if auth_method == AuthMethod::OAuthDeviceCode => {
                                // Compatibility with providers implementing the original
                                // Auru-proxied device-code endpoints.
                                auru_pm::start_device_flow(endpoint, "auru-pm-desktop".to_owned())
                            }
                            None => {
                                _ = this.update(cx, |this, cx| {
                                    this.auth_phase = AuthPhase::Failed(
                                        "The provider did not publish its OAuth configuration."
                                            .to_owned(),
                                    );
                                    cx.notify();
                                });
                                return;
                            }
                        },
                        Err(error) => {
                            _ = this.update(cx, |this, cx| {
                                this.auth_phase = AuthPhase::Failed(format!(
                                    "Couldn't read the provider's sign-in configuration: {error}"
                                ));
                                cx.notify();
                            });
                            return;
                        }
                    };
                    loop {
                        match receiver.try_recv() {
                            Ok(OAuthProgress::AuthorizationUrl(url)) => {
                                _ = this.update(cx, |_, cx| {
                                    cx.open_url(&url);
                                });
                            }
                            Ok(OAuthProgress::DeviceCode(code)) => {
                                _ = this.update(cx, |this, cx| {
                                    this.auth_phase = AuthPhase::DeviceCode {
                                        user_code: code.user_code,
                                        verification_uri: code
                                            .verification_uri_complete
                                            .unwrap_or(code.verification_uri),
                                    };
                                    cx.notify();
                                });
                            }
                            Ok(OAuthProgress::Pending) => {}
                            Ok(OAuthProgress::Token(token)) => {
                                let stored = auru_pm::token_store::store_provider_token(
                                    &provider_id,
                                    &token,
                                );
                                _ = this.update(cx, |this, cx| {
                                    this.auth_phase = match stored {
                                        Ok(()) => AuthPhase::Complete {
                                            detail: "The provider confirmed this device."
                                                .to_owned(),
                                        },
                                        Err(error) => AuthPhase::Failed(format!(
                                            "Signed in, but couldn't store the token: {error}"
                                        )),
                                    };
                                    cx.notify();
                                });
                                return;
                            }
                            Ok(OAuthProgress::Credential(credential)) => {
                                let stored = auru_pm::token_store::store_provider_credential(
                                    &provider_id,
                                    &credential,
                                );
                                _ = this.update(cx, |this, cx| {
                                    this.auth_phase = match stored {
                                        Ok(()) => AuthPhase::Complete {
                                            detail: "The provider confirmed this browser sign-in."
                                                .to_owned(),
                                        },
                                        Err(error) => AuthPhase::Failed(format!(
                                            "Signed in, but couldn't store the credential: {error}"
                                        )),
                                    };
                                    cx.notify();
                                });
                                return;
                            }
                            Ok(OAuthProgress::Expired) => {
                                _ = this.update(cx, |this, cx| {
                                    this.auth_phase = AuthPhase::Failed(
                                        "The sign-in code expired. Start again for a new one."
                                            .to_owned(),
                                    );
                                    cx.notify();
                                });
                                return;
                            }
                            Ok(OAuthProgress::AccessDenied) => {
                                _ = this.update(cx, |this, cx| {
                                    this.auth_phase = AuthPhase::Failed(
                                        "The provider declined this sign-in.".to_owned(),
                                    );
                                    cx.notify();
                                });
                                return;
                            }
                            Ok(OAuthProgress::Error(error)) => {
                                _ = this.update(cx, |this, cx| {
                                    this.auth_phase = AuthPhase::Failed(error);
                                    cx.notify();
                                });
                                return;
                            }
                            Err(std::sync::mpsc::TryRecvError::Empty) => {
                                cx.background_executor().timer(OAUTH_POLL_INTERVAL).await;
                            }
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                _ = this.update(cx, |this, cx| {
                                    this.auth_phase = AuthPhase::Failed(
                                        "The provider ended sign-in unexpectedly.".to_owned(),
                                    );
                                    cx.notify();
                                });
                                return;
                            }
                        }
                    }
                })
                .detach();
            }
            AuthMethod::None => {
                if self.mark_provider_connected(provider_index).is_some() {
                    self.auth_phase = AuthPhase::Complete {
                        detail: "This provider does not require sign-in.".to_owned(),
                    };
                }
                cx.notify();
            }
        }
    }

    fn finish_provider_auth(
        &mut self,
        provider_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let detail = match &self.auth_phase {
            AuthPhase::Complete { detail } => detail.clone(),
            _ => "The provider is configured on this device.".to_owned(),
        };
        let Some(provider_name) = self.mark_provider_connected(provider_index) else {
            return;
        };

        self.overlay.clear();
        self.auth_phase = AuthPhase::Ready;
        self.refresh_remote_projects(cx);
        window.push_notification(
            Notification::success(detail).title(format!("{provider_name} configured")),
            cx,
        );
        cx.notify();
    }

    fn cancel_provider_auth(&mut self, cx: &mut Context<Self>) {
        self.auth_phase = AuthPhase::Ready;
        self.overlay.replace(Overlay::ProviderPicker);
        cx.notify();
    }

    /// Show a project, reading its file the first time it is looked at.
    ///
    /// The library lists hundreds of projects from folder names alone; tempo,
    /// key and the plugin list need the Live Set opened, which is several
    /// megabytes of gunzip apiece. Doing that on selection means the list
    /// appears at once and the cost is paid only for what someone opens.
    fn select_project(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected_project = index;
        cx.notify();
        self.load_project_history(index, cx);

        let Some(project) = self.projects.get(index) else {
            return;
        };
        if project.detail.is_some() {
            return;
        }
        let Some(live_set) = project.live_set.clone() else {
            return;
        };
        let id = project.id.clone();

        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move { Project::detail_for(&live_set) })
                .await;
            _ = this.update(cx, |this, cx| {
                // Matched by id rather than index: the list may have been
                // reloaded or re-sorted while the file was being read.
                if let Some(project) = this.projects.iter_mut().find(|p| p.id == id) {
                    project.apply_detail(loaded);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn load_project_history(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(project) = self.projects.get(index) else {
            return;
        };
        if !project.versions.is_empty() {
            return;
        }
        let project_id = project.id.clone();
        let task = if let Some(project_path) = project.live_set.clone() {
            let Some(provider) = backend::primary_listing(&self.providers, &project_path) else {
                return;
            };
            cx.background_executor()
                .spawn(async move { backend::history(provider, project_path) })
        } else {
            let Some(remote) = project.remote.clone() else {
                return;
            };
            let Some(provider) = self
                .providers
                .iter()
                .find(|provider| provider.entry.id == remote.provider_id)
                .cloned()
            else {
                return;
            };
            cx.background_executor()
                .spawn(async move { backend::remote_history(provider, remote.handle) })
        };
        cx.spawn(async move |this, cx| {
            let Ok(history) = task.await else {
                return;
            };
            _ = this.update(cx, |this, cx| {
                if let Some(project) = this
                    .projects
                    .iter_mut()
                    .find(|project| project.id == project_id)
                {
                    project.apply_history(history);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn restore_version(
        &mut self,
        index: usize,
        commit_id: auru_pm::CommitId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.get(index) else {
            return;
        };
        let project_path = project.live_set.clone();
        let remote = project.remote.clone();
        let provider = if let Some(project_path) = project_path.as_deref() {
            backend::primary_listing(&self.providers, project_path)
        } else {
            remote.as_ref().and_then(|remote| {
                self.providers
                    .iter()
                    .find(|provider| provider.entry.id == remote.provider_id)
                    .cloned()
            })
        };
        let Some(provider) = provider else {
            window.push_notification(
                Notification::error("The provider for this history is not configured.")
                    .title("Can't restore this version"),
                cx,
            );
            return;
        };
        let project_name = project.name.clone();
        let project_file_name = project.file_name.clone();
        let project_id = project.id.clone();
        let remote_only = project_path.is_none();
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Restore into this folder".into()),
        });

        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(destination) = paths.into_iter().next() else {
                return;
            };
            if remote_only {
                _ = this.update_in(cx, |this, _, cx| {
                    if let Some(project) = this
                        .projects
                        .iter_mut()
                        .find(|project| project.id == project_id)
                    {
                        project.status = ProjectStatus::Syncing;
                        cx.notify();
                    }
                });
            }
            let task = cx.background_executor().spawn(async move {
                match (project_path, remote) {
                    (Some(project_path), _) => {
                        backend::restore(provider, project_path, commit_id, destination)
                    }
                    (None, Some(remote)) => backend::restore_remote(
                        provider,
                        remote.handle,
                        project_file_name,
                        commit_id,
                        destination,
                    ),
                    (None, None) => Err("The project has no provider identity.".to_owned()),
                }
            });
            let result = task.await;
            _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(report) => {
                        if remote_only {
                            this.state.add_project(&report.project_file);
                            this.state.save();
                            let restored = Project::read_from_disk(&report.project_file);
                            if let Some(index) = this
                                .projects
                                .iter()
                                .position(|project| project.id == project_id)
                            {
                                match restored {
                                    Some(restored) => {
                                        this.projects[index] = restored;
                                        this.selected_project = index;
                                    }
                                    None => {
                                        this.projects[index].status = ProjectStatus::NotDownloaded;
                                    }
                                }
                            }
                        }
                        let notification = if report.unavailable == 0 {
                            Notification::success(format!(
                                "{} captured file(s) restored to {}",
                                report.files_written,
                                report.project_file.display()
                            ))
                            .title(format!("{project_name} restored"))
                        } else {
                            Notification::warning(format!(
                                "Restored to {} · {} file(s) unavailable",
                                report.project_file.display(),
                                report.unavailable
                            ))
                            .title(format!("{project_name} partially restored"))
                        };
                        window.push_notification(notification, cx);
                    }
                    Err(message) => {
                        if remote_only
                            && let Some(project) = this
                                .projects
                                .iter_mut()
                                .find(|project| project.id == project_id)
                        {
                            project.status = ProjectStatus::NotDownloaded;
                        }
                        window.push_notification(
                            Notification::error(message)
                                .title(format!("Couldn't restore {project_name}")),
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn refresh_remote_projects(&mut self, cx: &mut Context<Self>) {
        if self.remote_refreshing {
            self.remote_refresh_pending = true;
            return;
        }
        let providers: Vec<ProviderListing> = self
            .providers
            .iter()
            .filter(|provider| provider.is_connected())
            .cloned()
            .collect();
        if providers.is_empty() {
            self.remote_discovery_errors.clear();
            return;
        }

        self.remote_refreshing = true;
        self.remote_refresh_pending = false;
        cx.notify();
        let task = cx.background_executor().spawn(async move {
            providers
                .into_iter()
                .map(|provider| {
                    let provider_id = provider.entry.id.clone();
                    (provider_id, backend::remote_catalogue(provider))
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let results = task.await;
            _ = this.update(cx, |this, cx| {
                let selected = this
                    .projects
                    .get(this.selected_project)
                    .map(|project| project.id.clone());
                this.remote_refreshing = false;
                this.remote_discovery_errors.clear();

                for (provider_id, result) in results {
                    let catalogue = match result {
                        Ok(catalogue) => catalogue,
                        Err(error) => {
                            this.remote_discovery_errors.push(error);
                            continue;
                        }
                    };
                    if catalogue.unavailable > 0 {
                        this.remote_discovery_errors.push(format!(
                            "{} project(s) on {provider_id} could not be read",
                            catalogue.unavailable
                        ));
                    }
                    replace_provider_projects(&mut this.projects, &provider_id, catalogue.projects);
                }
                let sort_order = this.sort_order();
                sort_projects(&mut this.projects, sort_order);
                this.selected_project = selected
                    .and_then(|id| this.projects.iter().position(|project| project.id == id))
                    .unwrap_or(0);
                if this.remote_refresh_pending {
                    this.refresh_remote_projects(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Re-read every watched folder and project from disk.
    ///
    /// The library is a view of the filesystem, so this is how it catches up
    /// with anything Live created or moved while Auru was open.
    fn reload_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let remote_only: Vec<Project> = self
            .projects
            .drain(..)
            .filter(|project| project.live_set.is_none())
            .collect();
        self.projects = load_library(&mut self.state);
        self.projects.extend(remote_only);
        let sort_order = self.sort_order();
        sort_projects(&mut self.projects, sort_order);
        self.selected_project = 0;
        self.route = Route::Library;
        self.overlay.clear();
        self.refresh_remote_projects(cx);

        let found = self.projects.len();
        let unbacked = self
            .projects
            .iter()
            .filter(|project| project.status == ProjectStatus::NeverBackedUp)
            .count();
        let message = match (found, unbacked) {
            (0, _) => "No projects yet. Watch a folder, or add one.".to_owned(),
            (found, 0) => format!("{found} project(s), all backed up."),
            (found, unbacked) => {
                format!("{found} project(s) · {unbacked} only on this computer.")
            }
        };
        window.push_notification(Notification::info(message).title("Library refreshed"), cx);
        cx.notify();
    }

    fn render_library(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let search_query = self.search_input.read(cx).value().to_lowercase();
        let attention_count = self
            .projects
            .iter()
            .filter(|project| project.status.needs_attention())
            .count();

        let sidebar = div()
            .flex()
            .h_full()
            .w(px(SIDEBAR_WIDTH))
            .flex_shrink_0()
            .flex_col()
            .border_r_1()
            .border_color(line())
            .bg(bg())
            .child(
                div()
                    .flex()
                    .h(px(50.0))
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(line())
                    .px_5()
                    .child(
                        div()
                            .text_size(px(11.0))
                            .font_weight(FontWeight::BOLD)
                            .text_color(bright())
                            .child("AURU PM"),
                    )
                    .child(
                        div()
                            .text_size(px(9.0))
                            .text_color(dim())
                            .child(format!("{} · ONLINE", self.display_name.to_uppercase())),
                    ),
            )
            .child(
                div()
                    .border_b_1()
                    .border_color(line())
                    .p_4()
                    .child(Input::new(&self.search_input).small().w_full()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_5()
                    .pb_2()
                    .pt_3()
                    .text_size(px(9.0))
                    .child(
                        // Shrinks first: the sort control is a fixed handful of
                        // characters, while this line grows with the library.
                        div()
                            .min_w_0()
                            .overflow_x_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(green())
                            .child(format!(
                                "[ LIBRARY · {} PROJECTS · {attention_count} NEED YOU ]",
                                self.projects.len()
                            )),
                    )
                    .child(div().flex_shrink_0().child(sort_menu(self.sort_order()))),
            )
            .child(
                div()
                    .id("backup-all")
                    .mx_5()
                    .mb_1()
                    .flex()
                    .h(px(34.0))
                    .cursor_pointer()
                    .items_center()
                    .border_1()
                    .border_color(amber())
                    .px_3()
                    .text_size(px(9.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(amber())
                    .hover(|this| this.bg(amber().opacity(0.1)))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.back_up_all(window, cx);
                    }))
                    .child("↑  BACK UP ALL CHANGES"),
            )
            .child(
                div()
                    .min_h_0()
                    .flex_1()
                    .child(self.render_project_list(&search_query, cx)),
            )
            .child(
                div()
                    .flex()
                    .h(px(44.0))
                    .items_center()
                    .justify_between()
                    .border_t_1()
                    .border_color(line())
                    .px_5()
                    .text_size(px(9.0))
                    .text_color(faint())
                    .child(add_project_menu())
                    .child(
                        div()
                            .id("open-settings")
                            .cursor_pointer()
                            .hover(|this| this.text_color(bright()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_settings(cx);
                            }))
                            .child("⚙ SETTINGS  ⌘,"),
                    ),
            );

        let detail = self
            .projects
            .get(self.selected_project)
            .map(|project| self.render_project_detail(self.selected_project, project, cx))
            .unwrap_or_else(|| {
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .text_color(dim())
                    .child("Select a project")
                    .into_any_element()
            });

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(bg())
            .child(div().flex().min_h_0().flex_1().child(sidebar).child(detail))
            .child(self.render_shortcuts_bar(cx))
            .into_any_element()
    }

    /// The scrolling list of projects.
    ///
    /// Virtualized because a real library is not a handful of projects: one
    /// measured 653, and building every row each frame made scrolling and
    /// typing visibly stutter. `uniform_list` asks only for the rows on
    /// screen, so the cost stops growing with the size of someone's library.
    ///
    /// Rows are a fixed height, which is what makes the uniform variant
    /// applicable — if they ever vary, this has to change with them.
    fn render_project_list(&self, search_query: &str, cx: &mut Context<Self>) -> AnyElement {
        // Which projects the search leaves, as indices into `projects`. The
        // list works in slots; this maps a slot back to the real project.
        let visible: Vec<usize> = self
            .projects
            .iter()
            .enumerate()
            .filter(|(_, project)| project.matches_search(search_query))
            .map(|(index, _)| index)
            .collect();

        if visible.is_empty() {
            return div()
                .flex()
                .h_full()
                .items_center()
                .justify_center()
                .px_5()
                .text_size(px(9.0))
                .text_color(faint())
                .child(if self.projects.is_empty() {
                    "No projects yet — watch a folder, or add one."
                } else {
                    "Nothing matches that search."
                })
                .into_any_element();
        }

        let this = cx.entity();
        let list = uniform_list("project-list", visible.len(), move |range, _window, cx| {
            this.update(cx, |manager, cx| {
                range
                    .filter_map(|slot| {
                        let index = *visible.get(slot)?;
                        let project = manager.projects.get(index)?;
                        Some(manager.render_project_row(index, project, cx))
                    })
                    .collect::<Vec<_>>()
            })
        })
        .track_scroll(&self.list_scroll)
        .size_full();

        // The scrollbar overlays the list rather than taking a column of its
        // own, so the rows keep the full sidebar width. `relative` is what
        // gives it something to position against.
        div()
            .relative()
            .size_full()
            .child(list)
            .vertical_scrollbar(&self.list_scroll)
            .into_any_element()
    }

    fn render_project_row(
        &self,
        index: usize,
        project: &Project,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = index == self.selected_project;
        let color = status_color(project.status);

        div()
            .id(format!("project-{}", project.id))
            .flex()
            // Every row fills the sidebar. A virtualized list does not stretch
            // its items, so without this each row sizes to its own text and
            // the selection outline, hover area and backup bar all end at a
            // different place down the list.
            .w_full()
            .h(px(56.0))
            .cursor_pointer()
            .items_center()
            .gap_3()
            .border_l_2()
            .border_color(if selected { color } else { bg() })
            .bg(if selected { selection() } else { bg() })
            .px_5()
            .hover(|this| this.bg(selection()))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.select_project(index, cx);
            }))
            .child(div().size(px(8.0)).rounded_full().bg(color))
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .flex_1()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .overflow_x_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .font_family(DISPLAY_FONT)
                            .text_size(px(15.0))
                            .text_color(bright())
                            .child(project.name.clone()),
                    )
                    .child(
                        div()
                            .overflow_x_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(9.0))
                            .text_color(color)
                            .child(project.list_status()),
                    )
                    .child(Self::render_backup_bar(project)),
            )
            .child(div().text_size(px(13.0)).text_color(faint()).child("›"))
            .into_any_element()
    }

    /// The thin bar under a project's name.
    ///
    /// Full strength while something is happening or while the project needs a
    /// decision; dimmed once it is simply backed up, where it confirms the
    /// state without competing for attention.
    fn render_backup_bar(project: &Project) -> AnyElement {
        let color = status_color(project.status);
        let prominent = project.backup_bar_is_prominent();
        let filled = project.backup_progress().clamp(0.0, 1.0);

        div()
            .h(px(2.0))
            .w_full()
            .bg(line())
            .child(div().h_full().w(relative(filled)).bg(if prominent {
                color
            } else {
                color.opacity(0.28)
            }))
            .into_any_element()
    }

    fn render_project_detail(
        &self,
        index: usize,
        project: &Project,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let color = status_color(project.status);
        let action = project.status.action();
        let action_color = if project.status == ProjectStatus::NotDownloaded {
            green()
        } else {
            color
        };
        let mut primary_action = div()
            .id(format!("project-primary-action-{}", project.id))
            .flex()
            .h(px(38.0))
            .min_w(px(148.0))
            .items_center()
            .justify_center()
            .gap_2()
            .bg(action_color)
            .px_4()
            .text_size(px(9.0))
            .font_weight(FontWeight::BOLD)
            .text_color(bg())
            .child(action.label());

        if action == ProjectAction::None {
            primary_action = primary_action
                .bg(action_color.opacity(0.18))
                .text_color(action_color)
                .child(Spinner::new().xsmall().color(action_color));
        } else {
            primary_action = primary_action
                .cursor_pointer()
                .hover(move |this| this.bg(action_color.opacity(0.82)))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.handle_project_action(index, window, cx);
                }));
        }

        let header = div()
            .flex()
            .items_start()
            .justify_between()
            .gap_5()
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .flex_col()
                    .gap_2()
                    .child(div().text_size(px(9.0)).text_color(faint()).child(format!(
                        "{} · {} · {}",
                        project.format_label(),
                        project.file_name.to_uppercase(),
                        project.size
                    )))
                    .child(
                        div()
                            .overflow_x_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .font_family(DISPLAY_FONT)
                            .text_size(px(34.0))
                            .text_color(bright())
                            .child(project.name.clone()),
                    ),
            )
            .child(div().flex().h(px(32.0)).items_center().gap_1().children(
                WAVEFORM_HEIGHTS.into_iter().map(|height| {
                    div()
                        .h(px(height))
                        .w(px(2.0))
                        .bg(waveform())
                        .into_any_element()
                }),
            ));

        let banner = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_5()
            .border_1()
            .border_color(color.opacity(0.35))
            .bg(color.opacity(0.06))
            .p_4()
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(8.0))
                            .text_color(color)
                            .child(project.status.label().to_uppercase()),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .font_weight(FontWeight::BOLD)
                            .text_color(color)
                            .child(project.status_headline()),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(dim())
                            .child(project.status_explanation()),
                    ),
            )
            .child(primary_action);

        let mut info_grid = div().flex().flex_col().border_1().border_color(line());

        // What the project *is*, when its detail has been read. Shown first
        // because it is what tells someone which project they are looking at.
        if let Some(detail) = &project.detail {
            info_grid = info_grid
                .child(
                    div()
                        .flex()
                        .child(info_cell("TEMPO", detail.tempo_line(), bright()))
                        .child(info_cell("KEY", detail.key_line(), bright())),
                )
                .child(
                    div()
                        .flex()
                        .border_t_1()
                        .border_color(line())
                        .child(info_cell("TRACKS", detail.tracks_line(), bright()))
                        .child(info_cell("LENGTH", detail.length_line(), bright())),
                )
                .child(
                    div()
                        .flex()
                        .border_t_1()
                        .border_color(line())
                        .child(info_cell("MADE WITH", detail.made_with(), bright()))
                        .child(info_cell("FILES", detail.files_line(), bright())),
                );
        }

        let backup_row_border = if project.detail.is_some() { 1.0 } else { 0.0 };
        let mut backup_row = div().flex();
        if backup_row_border > 0.0 {
            backup_row = backup_row.border_t_1().border_color(line());
        }
        info_grid = info_grid
            .child(
                backup_row
                    .child(info_cell(
                        "SAFE COPY LIVES ON",
                        "Auru Cloud · eu-west",
                        bright(),
                    ))
                    .child(info_cell(
                        "LAST CHECKED",
                        "Today, 18:42 · verified",
                        bright(),
                    )),
            )
            .child(
                div()
                    .flex()
                    .border_t_1()
                    .border_color(line())
                    .child(info_cell(
                        "LATEST SAFE VERSION",
                        project.safe_version.clone(),
                        bright(),
                    ))
                    .child(info_cell(
                        "ON THIS COMPUTER",
                        project.local_inventory.clone(),
                        color,
                    )),
            );

        // Only rendered when something is actually missing. A project whose
        // plugins are all present should say nothing at all rather than show
        // an empty section implying there is something to deal with.
        let missing_plugins = (!project.missing_plugins.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .child(section_label("[ PLUGINS NOT ON THIS COMPUTER ]"))
                .children(project.missing_plugins.iter().enumerate().map(
                    |(plugin_index, plugin)| {
                        let link = plugin.link.clone();
                        let mut row = div()
                            .flex()
                            .min_h(px(40.0))
                            .items_center()
                            .justify_between()
                            .gap_4()
                            .border_t_1()
                            .border_color(line())
                            .px_1()
                            .child(
                                div()
                                    .flex()
                                    .min_w_0()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .overflow_x_hidden()
                                            .whitespace_nowrap()
                                            .text_ellipsis()
                                            .text_size(px(10.0))
                                            .text_color(ink())
                                            .child(plugin.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(9.0))
                                            .text_color(faint())
                                            .child(plugin.detail_line()),
                                    ),
                            );

                        if let Some(url) = link {
                            row = row.child(
                                div()
                                    .id(format!("plugin-link-{}-{plugin_index}", project.id))
                                    .flex_shrink_0()
                                    .cursor_pointer()
                                    .text_size(px(9.0))
                                    .text_color(blue())
                                    .hover(|this| this.text_color(bright()))
                                    .on_click(cx.listener(move |_, _, window, cx| {
                                        // Opening the maker's page is as far as
                                        // this goes: obtaining and authorizing a
                                        // plugin is between the person and its
                                        // vendor, and nothing here touches that.
                                        cx.open_url(&url);
                                        window.push_notification(
                                            Notification::info(
                                                "Opened the plugin maker's page in your browser.",
                                            )
                                            .title("Where to get it"),
                                            cx,
                                        );
                                    }))
                                    .child("WHERE TO GET IT  ↗"),
                            );
                        } else {
                            row = row.child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(px(9.0))
                                    .text_color(faint())
                                    .child("SEARCH BY NAME"),
                            );
                        }

                        row.into_any_element()
                    },
                ))
                .child(
                    div()
                        .border_t_1()
                        .border_color(line())
                        .pt_3()
                        .px_1()
                        .text_size(px(9.0))
                        .text_color(dim())
                        .child(PLUGIN_SETTINGS_REASSURANCE),
                )
        });

        let versions = div()
            .flex()
            .flex_col()
            .child(section_label("[ RECENT VERSIONS ]"))
            .children(
                project
                    .versions
                    .iter()
                    .enumerate()
                    .map(|(version_index, version)| {
                        let commit_id = version.id;
                        div()
                            .flex()
                            .min_h(px(36.0))
                            .items_center()
                            .justify_between()
                            .gap_4()
                            .border_t_1()
                            .border_color(line())
                            .px_1()
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_x_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_size(px(10.0))
                                    .text_color(ink())
                                    .child(format!("{} · {}", version.version, version.summary)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_shrink_0()
                                    .gap_2()
                                    .text_size(px(9.0))
                                    .child(
                                        div()
                                            .text_color(faint())
                                            .child(format!("{} ·", version.created_at)),
                                    )
                                    .child(
                                        div()
                                            .id(format!("restore-{}-{version_index}", project.id))
                                            .cursor_pointer()
                                            .text_color(green())
                                            .hover(|this| this.text_color(bright()))
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.restore_version(index, commit_id, window, cx);
                                            }))
                                            .child("RESTORE"),
                                    ),
                            )
                            .into_any_element()
                    }),
            );

        div()
            .flex()
            .min_w_0()
            .flex_1()
            .flex_col()
            .bg(bg())
            .child(
                div().min_h_0().flex_1().overflow_y_scrollbar().p_8().child(
                    div()
                        .flex()
                        .min_h_full()
                        .flex_col()
                        .gap_5()
                        .child(header)
                        .child(banner)
                        .child(info_grid)
                        .child(
                            div()
                                .overflow_x_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_size(px(8.0))
                                .text_color(faint())
                                .child(project.displayed_path()),
                        )
                        .children(missing_plugins)
                        .child(versions),
                ),
            )
            .child(
                div()
                    .flex()
                    .h(px(44.0))
                    .items_center()
                    .justify_between()
                    .border_t_1()
                    .border_color(line())
                    .px_8()
                    .text_size(px(9.0))
                    .text_color(faint())
                    .child("SHOWING UP TO 50 VERSIONS")
                    .child(
                        div()
                            .id(format!("open-project-footer-{}", project.id))
                            .cursor_pointer()
                            .hover(|this| this.text_color(bright()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let Some(path) = this
                                    .projects
                                    .get(index)
                                    .and_then(|project| project.live_set.as_deref())
                                else {
                                    return;
                                };
                                if let Err(message) = backend::open_project(path) {
                                    window.push_notification(
                                        Notification::error(message)
                                            .title("Couldn't open the project"),
                                        cx,
                                    );
                                }
                            }))
                            .child(project.open_label()),
                    ),
            )
            .into_any_element()
    }

    fn render_shortcuts_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .h(px(28.0))
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(line())
            .bg(rgb(0x0c0f0e))
            .px_5()
            .text_size(px(8.0))
            .text_color(faint())
            .child("[ LIBRARY ]")
            .child(
                div().flex().gap_6().child(
                    div()
                        .id("shortcut-refresh")
                        .cursor_pointer()
                        .hover(|this| this.text_color(bright()))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.reload_library(window, cx);
                        }))
                        .child("↺ REFRESH"),
                ),
            )
            .into_any_element()
    }

    fn render_onboarding(&self, cx: &mut Context<Self>) -> AnyElement {
        let (position, total) = self.onboarding_step.position();
        let can_continue = self.onboarding_step != OnboardingStep::Profile
            || !self.display_name_input.read(cx).value().trim().is_empty();
        let (title, detail, body) = match self.onboarding_step {
            OnboardingStep::Profile => (
                "Hello. What should we call you?",
                "This name appears in project history. It stays with your local profile — no login or account is created.",
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .border_1()
                    .border_color(line())
                    .bg(panel())
                    .p_5()
                    .child(
                        div()
                            .text_size(px(9.0))
                            .text_color(faint())
                            .child("DISPLAY NAME"),
                    )
                    .child(Input::new(&self.display_name_input).w_full())
                    .into_any_element(),
            ),
            OnboardingStep::Provider => {
                let connected = self
                    .providers
                    .iter()
                    .filter(|provider| provider.is_connected())
                    .count();
                (
                    "Where should your backups live?",
                    "Connect a hosted provider, a machine on your network, or an ordinary local folder. You can skip this and add one later.",
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .border_1()
                        .border_color(if connected > 0 { green() } else { line() })
                        .bg(panel())
                        .p_5()
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(if connected > 0 { green() } else { dim() })
                                .child(if connected == 0 {
                                    "NO BACKUP DESTINATION CONNECTED".to_owned()
                                } else {
                                    format!(
                                        "{connected} DESTINATION{} CONNECTED",
                                        if connected == 1 { "" } else { "S" }
                                    )
                                }),
                        )
                        .child(
                            div()
                                .id("onboarding-choose-provider")
                                .flex()
                                .h(px(42.0))
                                .items_center()
                                .justify_center()
                                .border_1()
                                .border_color(green())
                                .cursor_pointer()
                                .text_size(px(9.0))
                                .text_color(green())
                                .hover(|this| this.bg(selection()))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.overlay
                                        .show(OverlayHost::Main, Overlay::ProviderPicker);
                                    cx.notify();
                                }))
                                .child(if connected == 0 {
                                    "CHOOSE A BACKUP DESTINATION"
                                } else {
                                    "ADD ANOTHER DESTINATION"
                                }),
                        )
                        .into_any_element(),
                )
            }
            OnboardingStep::Music => {
                let watched = self.state.watched_folders.len();
                (
                    "Where do you keep your projects?",
                    "Choose a folder and Auru will find supported projects inside it. Scanning only looks; nothing is uploaded.",
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .border_1()
                        .border_color(if watched > 0 { green() } else { line() })
                        .bg(panel())
                        .p_5()
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(if watched > 0 { green() } else { dim() })
                                .child(if watched == 0 {
                                    "NO PROJECT FOLDERS WATCHED".to_owned()
                                } else {
                                    format!(
                                        "{watched} FOLDER{} WATCHED",
                                        if watched == 1 { "" } else { "S" }
                                    )
                                }),
                        )
                        .child(
                            div()
                                .id("onboarding-watch-folder")
                                .flex()
                                .h(px(42.0))
                                .items_center()
                                .justify_center()
                                .border_1()
                                .border_color(green())
                                .cursor_pointer()
                                .text_size(px(9.0))
                                .text_color(green())
                                .hover(|this| this.bg(selection()))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.watch_another_folder(window, cx);
                                }))
                                .child(if self.scanning {
                                    "SCANNING…"
                                } else if watched == 0 {
                                    "CHOOSE YOUR PROJECTS FOLDER"
                                } else {
                                    "WATCH ANOTHER FOLDER"
                                }),
                        )
                        .into_any_element(),
                )
            }
        };
        let mut continue_button = div()
            .id("continue-onboarding")
            .flex()
            .h(px(42.0))
            .min_w(px(180.0))
            .items_center()
            .justify_center()
            .bg(if can_continue {
                green()
            } else {
                green().opacity(0.25)
            })
            .px_5()
            .text_size(px(9.0))
            .font_weight(FontWeight::BOLD)
            .text_color(if can_continue { bg() } else { dim() })
            .child(if self.onboarding_step == OnboardingStep::Music {
                "OPEN MY LIBRARY →"
            } else {
                "CONTINUE →"
            });

        if can_continue {
            continue_button = continue_button
                .cursor_pointer()
                .hover(|this| this.bg(green().opacity(0.82)))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.advance_onboarding(window, cx);
                }));
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(bg())
            .child(
                div()
                    .flex()
                    .h(px(64.0))
                    .items_center()
                    .justify_between()
                    .px_8()
                    .text_size(px(9.0))
                    .text_color(faint())
                    .child("AURU PM · SETUP")
                    .child(format!("{position} / {total}")),
            )
            .child(
                div().flex().flex_1().items_center().justify_center().child(
                    div()
                        .flex()
                        .w(px(560.0))
                        .flex_col()
                        .gap_5()
                        .child(
                            div()
                                .font_family(DISPLAY_FONT)
                                .text_size(px(38.0))
                                .text_color(bright())
                                .child(title),
                        )
                        .child(
                            div()
                                .max_w(px(500.0))
                                .text_size(px(10.0))
                                .line_height(relative(1.6))
                                .text_color(dim())
                                .child(detail),
                        )
                        .child(body)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .id("previous-onboarding")
                                        .cursor_pointer()
                                        .text_size(px(9.0))
                                        .text_color(faint())
                                        .hover(|this| this.text_color(bright()))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.previous_onboarding_step(cx);
                                        }))
                                        .child(
                                            if self.onboarding_step == OnboardingStep::Profile {
                                                "← BACK TO LIBRARY"
                                            } else {
                                                "← BACK"
                                            },
                                        ),
                                )
                                .child(continue_button),
                        ),
                ),
            )
            .into_any_element()
    }

    /// The settings body, built from the shared settings component.
    ///
    /// Field closures run with only an `App`, not this view's context, so each
    /// captures a handle to the view and goes back through it. `Entity::update`
    /// hands back a real `Context<Self>`, which is what lets the custom items
    /// below reuse the existing render methods unchanged.
    fn settings_component(&self, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.entity();

        let switch = |read: fn(&Self) -> bool, write: fn(&mut Self, bool)| {
            let read_handle = this.clone();
            let write_handle = this.clone();
            SettingField::switch(
                move |cx: &App| read(read_handle.read(cx)),
                move |value: bool, cx: &mut App| {
                    write_handle.update(cx, |this, cx| {
                        write(this, value);
                        cx.notify();
                    });
                },
            )
        };

        let backups = SettingPage::new("Backups")
            .description("Where your work is copied to, and when.")
            .group(
                SettingGroup::new()
                    .title("Where backups live")
                    .description("One safe place away from this computer. You can add more.")
                    .item(SettingItem::render({
                        let this = this.clone();
                        move |_, _window, cx: &mut App| {
                            this.update(cx, |this, cx| this.render_provider_settings(cx))
                        }
                    })),
            )
            .group(
                SettingGroup::new()
                    .title("Backup behaviour")
                    .item(
                        SettingItem::new(
                            "Back up automatically after changes",
                            switch(
                                |this| this.automatic_backups,
                                |this, value| {
                                    this.automatic_backups = value;
                                    this.state.automatic_backups = value;
                                    this.state.save();
                                },
                            ),
                        )
                        .description("Waits for five quiet minutes, then copies in the background")
                        .keywords(["automatic", "background", "idle"]),
                    )
                    .item(
                        SettingItem::new(
                            "Verify every copy after upload",
                            switch(
                                |this| this.verify_uploads,
                                |this, value| {
                                    this.verify_uploads = value;
                                    this.state.verify_uploads = value;
                                    this.state.save();
                                },
                            ),
                        )
                        .description(
                            "Re-reads the stored files and checks nothing is missing or damaged",
                        )
                        .keywords(["verify", "checksum", "integrity"]),
                    )
                    .item(
                        SettingItem::new("Old versions", {
                            let read_handle = this.clone();
                            let write_handle = this.clone();
                            SettingField::dropdown(
                                VersionRetention::ALL
                                    .into_iter()
                                    .map(|option| {
                                        (
                                            SharedString::from(option.key()),
                                            SharedString::from(option.label()),
                                        )
                                    })
                                    .collect(),
                                move |cx: &App| {
                                    SharedString::from(read_handle.read(cx).version_retention.key())
                                },
                                move |value: SharedString, cx: &mut App| {
                                    write_handle.update(cx, |this, cx| {
                                        this.version_retention = VersionRetention::from_key(&value);
                                        this.state.version_retention =
                                            this.version_retention.key().to_owned();
                                        this.state.save();
                                        cx.notify();
                                    });
                                },
                            )
                        })
                        .description(
                            "Applied after each successful backup; removed versions cannot be restored",
                        )
                        .keywords(["history", "versions", "prune"]),
                    ),
            );

        let music = SettingPage::new("Your music")
            .description("The folders Auru looks in for projects.")
            .group(
                SettingGroup::new()
                    .title("Where your music lives")
                    .description("Scanning only looks — nothing is uploaded until you choose.")
                    .item(SettingItem::render({
                        let this = this.clone();
                        move |_, _window, cx: &mut App| {
                            this.update(cx, |this, cx| this.render_watched_folders(cx))
                        }
                    }))
                    .item(SettingItem::render({
                        let this = this.clone();
                        move |_, _window, cx: &mut App| {
                            this.update(cx, |this, cx| this.render_path_aliases(cx))
                        }
                    })),
            );

        let appearance = SettingPage::new("Appearance").group(
            SettingGroup::new().title("Theme").item(
                SettingItem::new("Theme", {
                    let read_handle = this.clone();
                    let write_handle = this.clone();
                    SettingField::dropdown(
                        // Two options because the underlying theme really has
                        // two. A third label that behaved identically to one
                        // of these would be a control that lies.
                        [Appearance::Night, Appearance::Day]
                            .into_iter()
                            .map(|option| {
                                (
                                    SharedString::from(option.key()),
                                    SharedString::from(option.label()),
                                )
                            })
                            .collect(),
                        move |cx: &App| SharedString::from(read_handle.read(cx).appearance.key()),
                        move |value: SharedString, cx: &mut App| {
                            let appearance = Appearance::from_key(&value);
                            write_handle.update(cx, |this, cx| {
                                this.apply_appearance(appearance, cx);
                            });
                        },
                    )
                })
                .description("Applies to the whole app straight away"),
            ),
        );

        let profile = SettingPage::new("Profile").group(
            SettingGroup::new()
                .title("Local profile")
                .description("Display name only — providers authenticate separately.")
                .item(SettingItem::render({
                    let this = this.clone();
                    move |_, _window, cx: &mut App| {
                        this.update(cx, |this, cx| this.render_profile_settings(cx))
                    }
                })),
        );

        Settings::new("auru-settings")
            .sidebar_width(px(176.0))
            .pages(vec![backups, music, appearance, profile])
            .into_any_element()
    }

    /// Provider list plus the add-provider action.
    fn render_provider_settings(&mut self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(
                self.providers
                    .iter()
                    .enumerate()
                    .map(|(index, provider)| {
                        self.render_settings_provider(index, provider, cx)
                    }),
            )
            .child(
                div()
                    .id("add-provider")
                    .flex()
                    .h(px(44.0))
                    .cursor_pointer()
                    .items_center()
                    .border_1()
                    .border_color(line())
                    .px_4()
                    .text_size(px(8.0))
                    .text_color(faint())
                    .hover(|this| this.border_color(green()).text_color(green()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.overlay
                            .show(OverlayHost::Settings, Overlay::ProviderPicker);
                        cx.notify();
                    }))
                    .child("＋ ADD ANOTHER PROVIDER FROM THE CATALOG…"),
            )
            .into_any_element()
    }

    /// Display name and the recover-another-device action.
    fn render_profile_settings(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let remote_projects = self
            .projects
            .iter()
            .filter(|project| project.live_set.is_none())
            .count();
        let recovery_status = if self.remote_refreshing {
            "Checking connected providers…".to_owned()
        } else if remote_projects == 0 {
            "No provider-only projects found.".to_owned()
        } else if remote_projects == 1 {
            "1 project is ready to download from its provider.".to_owned()
        } else {
            format!("{remote_projects} projects are ready to download from their providers.")
        };
        let recovery_errors = self.remote_discovery_errors.join(" · ");
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(9.0))
                            .text_color(dim())
                            .child("Shown against the versions you save"),
                    )
                    .child(Input::new(&self.display_name_setting).w_full())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_4()
                            .child(
                                div()
                                    .text_size(px(8.0))
                                    .text_color(faint())
                                    .child(format!("Currently saving as {}", self.display_name)),
                            )
                            .child(
                                div()
                                    .id("save-display-name")
                                    .flex_shrink_0()
                                    .cursor_pointer()
                                    .border_1()
                                    .border_color(line())
                                    .px_4()
                                    .py_2()
                                    .text_size(px(8.0))
                                    .text_color(green())
                                    .hover(|this| this.border_color(green()))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.commit_display_name(cx);
                                        window.push_notification(
                                            Notification::success(format!(
                                                "New versions will be saved as {}.",
                                                this.display_name
                                            ))
                                            .title("Name updated"),
                                            cx,
                                        );
                                    }))
                                    .child("SAVE NAME"),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .border_t_1()
                    .border_color(line())
                    .pt_4()
                    .child(
                        div()
                            .text_size(px(9.0))
                            .line_height(relative(1.5))
                            .text_color(dim())
                            .child(recovery_status),
                    )
                    .when(!recovery_errors.is_empty(), |this| {
                        this.child(
                            div()
                                .text_size(px(8.0))
                                .line_height(relative(1.5))
                                .text_color(amber())
                                .child(recovery_errors),
                        )
                    })
                    .child(
                        div()
                            .id("refresh-provider-projects")
                            .flex()
                            .h(px(36.0))
                            .cursor_pointer()
                            .items_center()
                            .justify_center()
                            .border_1()
                            .border_color(line())
                            .text_size(px(8.0))
                            .text_color(green())
                            .hover(|this| this.border_color(green()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.refresh_remote_projects(cx);
                            }))
                            .child(if self.remote_refreshing {
                                "CHECKING PROVIDERS…"
                            } else {
                                "REFRESH PROVIDER PROJECTS"
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_settings_provider(
        &self,
        index: usize,
        provider: &ProviderListing,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let connected = provider.availability == ProviderAvailability::Connected;
        let mut row = div()
            .id(format!("settings-provider-{}", provider.entry.id))
            .flex()
            .min_h(px(54.0))
            .items_center()
            .justify_between()
            .gap_4()
            .border_1()
            .border_color(if connected {
                green().opacity(0.45)
            } else {
                line()
            })
            .px_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(8.0))
                            .rounded_full()
                            .border_1()
                            .border_color(if connected { green() } else { faint() })
                            .bg(if connected { green() } else { panel() }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .font_family(DISPLAY_FONT)
                                    .text_size(px(14.0))
                                    .text_color(bright())
                                    .child(provider.entry.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(8.0))
                                    .text_color(faint())
                                    .child(provider.detail.clone()),
                            )
                            // What signing in will involve, before the person
                            // commits to a provider rather than after.
                            .child(
                                div()
                                    .text_size(px(8.0))
                                    .text_color(if connected { faint() } else { blue() })
                                    .child(provider.auth_hint().summary),
                            ),
                    ),
            )
            .child(
                div()
                    .text_size(px(8.0))
                    .text_color(if connected { green() } else { faint() })
                    .child(provider.availability.label()),
            );

        if !connected {
            row = row
                .cursor_pointer()
                .hover(|this| this.border_color(green()).bg(selection()))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_provider(index, OverlayHost::Settings, window, cx);
                }));
        }

        row.into_any_element()
    }

    fn render_conflict_resolver(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(pending) = self.pending_conflict.as_ref() else {
            return overlay_backdrop(
                div()
                    .border_1()
                    .border_color(line())
                    .bg(panel())
                    .p_6()
                    .text_color(bright())
                    .child("The conflict details are no longer available."),
            );
        };
        let rows = pending
            .backup
            .conflicts()
            .iter()
            .enumerate()
            .map(|(index, conflict)| {
                (
                    index,
                    conflict.path.clone(),
                    conflict_value_label(&conflict.local),
                    conflict_value_label(&conflict.remote),
                    pending.choices[index],
                )
            })
            .collect::<Vec<_>>();
        let project_name = pending.project_name.clone();

        let panel =
            div()
                .flex()
                .w(px(720.0))
                .max_h(px(680.0))
                .flex_col()
                .border_1()
                .border_color(line())
                .bg(panel())
                .shadow_lg()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(line())
                        .px_5()
                        .py_4()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .font_family(DISPLAY_FONT)
                                        .text_size(px(24.0))
                                        .text_color(bright())
                                        .child(format!("Resolve {project_name}")),
                                )
                                .child(div().text_size(px(8.0)).text_color(dim()).child(
                                    "Choose which value to keep for every conflicting field.",
                                )),
                        )
                        .child(
                            div()
                                .id("close-conflict-resolver")
                                .cursor_pointer()
                                .text_color(faint())
                                .hover(|this| this.text_color(bright()))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.overlay.clear();
                                    cx.notify();
                                }))
                                .child("×"),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .p_5()
                        .overflow_y_scrollbar()
                        .children(
                            rows.into_iter()
                                .map(|(index, path, local, remote, choice)| {
                                    let choice_button = |label: &'static str,
                                                 value: String,
                                                 candidate: ConflictChoice,
                                                 selected: bool,
                                                 cx: &mut Context<Self>| {
                                div()
                                    .id((label, index))
                                    .flex()
                                    .min_w_0()
                                    .flex_1()
                                    .flex_col()
                                    .gap_2()
                                    .border_1()
                                    .border_color(if selected { green() } else { line() })
                                    .bg(if selected { selection() } else { bg() })
                                    .p_3()
                                    .cursor_pointer()
                                    .hover(|this| this.border_color(green()))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(pending) = this.pending_conflict.as_mut()
                                            && let Some(choice) = pending.choices.get_mut(index)
                                        {
                                            *choice = candidate;
                                        }
                                        cx.notify();
                                    }))
                                    .child(
                                        div()
                                            .text_size(px(8.0))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(if selected { green() } else { faint() })
                                            .child(label),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(9.0))
                                            .line_height(relative(1.4))
                                            .text_color(ink())
                                            .child(value),
                                    )
                            };
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .child(
                                            div().text_size(px(8.0)).text_color(blue()).child(path),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .gap_3()
                                                .child(choice_button(
                                                    "KEEP THIS COMPUTER",
                                                    local,
                                                    ConflictChoice::Local,
                                                    choice == ConflictChoice::Local,
                                                    cx,
                                                ))
                                                .child(choice_button(
                                                    "KEEP PROVIDER",
                                                    remote,
                                                    ConflictChoice::Remote,
                                                    choice == ConflictChoice::Remote,
                                                    cx,
                                                )),
                                        )
                                        .into_any_element()
                                }),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_3()
                        .border_t_1()
                        .border_color(line())
                        .p_5()
                        .child(
                            div()
                                .id("cancel-conflict-resolution")
                                .cursor_pointer()
                                .px_4()
                                .py_2()
                                .text_size(px(8.0))
                                .text_color(faint())
                                .hover(|this| this.text_color(bright()))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.overlay.clear();
                                    cx.notify();
                                }))
                                .child("NOT NOW"),
                        )
                        .child(
                            div()
                                .id("commit-conflict-resolution")
                                .cursor_pointer()
                                .border_1()
                                .border_color(green())
                                .bg(green())
                                .px_5()
                                .py_2()
                                .text_size(px(8.0))
                                .font_weight(FontWeight::BOLD)
                                .text_color(bg())
                                .hover(|this| this.bg(green().opacity(0.82)))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.resolve_pending_conflict(window, cx);
                                }))
                                .child("SAVE RESOLVED VERSION"),
                        ),
                );
        overlay_backdrop(panel)
    }

    fn resolve_pending_conflict(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_conflict.as_ref() else {
            return;
        };
        let project_id = pending.project_id.clone();
        let project_name = pending.project_name.clone();
        let conflict = pending.backup.clone();
        let choices = pending.choices.clone();
        if let Some(project) = self
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
        {
            project.status = ProjectStatus::Syncing;
            project.sync_progress = 0.0;
        }
        self.overlay.clear();
        cx.notify();

        let resolution = cx
            .background_executor()
            .spawn(async move { backend::resolve_backup(&conflict, choices) });
        cx.spawn_in(window, async move |this, cx| {
            let result = resolution.await;
            _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(backend::BackupResult::Committed(receipt)) => {
                        if let Some(project) = this
                            .projects
                            .iter_mut()
                            .find(|project| project.id == project_id)
                        {
                            project.finish_transfer(receipt.history);
                        }
                        this.pending_conflict = None;
                        window.push_notification(
                            Notification::success(
                                "Your choices were merged with the latest provider version.",
                            )
                            .title(format!("{project_name} backed up")),
                            cx,
                        );
                    }
                    Ok(backend::BackupResult::NeedsResolution(conflict)) => {
                        let count = conflict.conflicts().len();
                        if let Some(project) = this
                            .projects
                            .iter_mut()
                            .find(|project| project.id == project_id)
                        {
                            project.status = ProjectStatus::Conflicted;
                            project.sync_progress = 0.0;
                        }
                        this.pending_conflict = Some(PendingConflict {
                            project_id,
                            project_name: project_name.clone(),
                            choices: vec![ConflictChoice::Local; count],
                            backup: conflict,
                        });
                        this.overlay
                            .show(OverlayHost::Main, Overlay::ConflictResolver);
                        window.push_notification(
                            Notification::warning(
                                "The provider changed again. Review the refreshed fields.",
                            )
                            .title(format!("{project_name} changed while resolving")),
                            cx,
                        );
                    }
                    Ok(backend::BackupResult::NeedsReview(problems)) => {
                        if let Some(project) = this
                            .projects
                            .iter_mut()
                            .find(|project| project.id == project_id)
                        {
                            project.status = ProjectStatus::Conflicted;
                            project.sync_progress = 0.0;
                        }
                        this.pending_conflict = None;
                        window.push_notification(
                            Notification::warning(format!(
                                "Those choices produce {problems} project integrity problem(s). \
                                 Your original version remains stashed safely."
                            ))
                            .title(format!("{project_name} still needs review")),
                            cx,
                        );
                    }
                    Err(message) => {
                        if let Some(project) = this
                            .projects
                            .iter_mut()
                            .find(|project| project.id == project_id)
                        {
                            project.status = ProjectStatus::Conflicted;
                            project.sync_progress = 0.0;
                        }
                        window.push_notification(
                            Notification::error(message)
                                .title(format!("Couldn't resolve {project_name}")),
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn render_provider_picker(
        &self,
        overlay_host: OverlayHost,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let provider_rows = self.providers.iter().enumerate().map(|(index, provider)| {
            let connected = provider.availability == ProviderAvailability::Connected;
            let requires_auth = provider.requires_authentication();
            let mut row = div()
                .id(format!("catalog-provider-{}", provider.entry.id))
                .flex()
                .min_h(px(66.0))
                .items_center()
                .justify_between()
                .gap_4()
                .border_1()
                .border_color(line())
                .px_4()
                .child(
                    div()
                        .flex()
                        .min_w_0()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .font_family(DISPLAY_FONT)
                                .text_size(px(15.0))
                                .text_color(bright())
                                .child(provider.entry.name.clone()),
                        )
                        .child(
                            div()
                                .overflow_x_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_size(px(8.0))
                                .text_color(faint())
                                .child(provider.entry.description.clone()),
                        ),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(8.0))
                        .text_color(if connected { green() } else { dim() })
                        .child(if connected {
                            "CONNECTED"
                        } else if requires_auth {
                            "SIGN-IN REQUIRED →"
                        } else {
                            "ADD DIRECTLY →"
                        }),
                );

            if !connected {
                row = row
                    .cursor_pointer()
                    .hover(|this| this.border_color(green()).bg(selection()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_provider(index, overlay_host, window, cx);
                    }));
            }
            row.into_any_element()
        });

        let panel = div()
            .flex()
            .w(px(600.0))
            .flex_col()
            .border_1()
            .border_color(line())
            .bg(panel())
            .shadow_lg()
            .child(
                div()
                    .flex()
                    .h(px(50.0))
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(line())
                    .px_5()
                    .text_size(px(10.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(bright())
                    .child("ADD A PROVIDER")
                    .child(
                        div()
                            .id("close-provider-picker")
                            .cursor_pointer()
                            .text_color(faint())
                            .hover(|this| this.text_color(bright()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.overlay.clear();
                                cx.notify();
                            }))
                            .child("×"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .p_5()
                    .child(
                        div()
                            .font_family(DISPLAY_FONT)
                            .text_size(px(26.0))
                            .text_color(bright())
                            .child("Where should backups live?"),
                    )
                    .child(
                        div()
                            .text_size(px(9.0))
                            .line_height(relative(1.5))
                            .text_color(dim())
                            .child(
                                "The first-party catalog is shown below. Providers declare their own authentication method; Auru PM only starts sign-in when one requests it.",
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(8.0))
                            .text_color(faint())
                            .child(format!(
                                "{} · {AURU_REGISTRY_URL}",
                                self.catalog_state.label()
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .when(self.providers.is_empty(), |this| {
                                // Silence would read as "there are no providers",
                                // which is a different thing from "we could not
                                // ask". Both leave the list empty; only one is
                                // worth retrying, so they must not look alike.
                                this.child(
                                    div()
                                        .flex()
                                        .h(px(64.0))
                                        .items_center()
                                        .justify_center()
                                        .px_4()
                                        .text_size(px(9.0))
                                        .text_color(faint())
                                        .child(match self.catalog_state {
                                            CatalogState::Loading => {
                                                "Looking for providers…"
                                            }
                                            CatalogState::Unreachable => {
                                                "Couldn't reach the provider list. Add a local folder below — it needs no account."
                                            }
                                            _ => "No providers yet. Add a local folder below.",
                                        }),
                                )
                            })
                            .children(provider_rows),
                    )
                    .child(
                        div()
                            .id("add-local-provider")
                            .flex()
                            .h(px(44.0))
                            .cursor_pointer()
                            .items_center()
                            .border_1()
                            .border_color(line())
                            .px_4()
                            .text_size(px(8.0))
                            .text_color(faint())
                            .hover(|this| this.border_color(green()).text_color(green()))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.add_local_provider(window, cx);
                            }))
                            .child("＋ ADD A LOCAL FOLDER OR NAS  ·  NO ACCOUNT NEEDED"),
                    ),
            );

        overlay_backdrop(panel)
    }

    fn render_authentication(&self, provider_index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(provider) = self.providers.get(provider_index) else {
            return overlay_backdrop(
                div()
                    .border_1()
                    .border_color(line())
                    .bg(panel())
                    .p_6()
                    .text_color(bright())
                    .child("Provider unavailable"),
            );
        };
        let auth_method = provider.preferred_auth_method();
        let auth_hint = AuthHint::for_method(&auth_method);

        let phase_content = match &self.auth_phase {
            AuthPhase::Ready => {
                let mut content = div().flex().flex_col().gap_4().child(
                    div()
                        .text_size(px(9.0))
                        .line_height(relative(1.6))
                        .text_color(dim())
                        // The same hint shown against the provider in the
                        // picker, expanded — so what was promised there is
                        // what happens here.
                        .child(auth_hint.detail),
                );

                if auth_hint.accepts_credential {
                    content = content.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(8.0))
                                    .text_color(faint())
                                    .child("PERSONAL ACCESS TOKEN"),
                            )
                            .child(Input::new(&self.credential_input).mask_toggle().w_full()),
                    );
                }

                content
                    .child(
                        div()
                            .id("begin-provider-auth")
                            .flex()
                            .h(px(42.0))
                            .cursor_pointer()
                            .items_center()
                            .justify_center()
                            .bg(green())
                            .text_size(px(9.0))
                            .font_weight(FontWeight::BOLD)
                            .text_color(bg())
                            .hover(|this| this.bg(green().opacity(0.82)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.begin_provider_auth(provider_index, window, cx);
                            }))
                            .child(auth_hint.action),
                    )
                    .into_any_element()
            }
            AuthPhase::Waiting => div()
                .flex()
                .min_h(px(190.0))
                .flex_col()
                .items_center()
                .justify_center()
                .gap_4()
                .child(Spinner::new().color(green()))
                .child(
                    div()
                        .text_size(px(10.0))
                        .font_weight(FontWeight::BOLD)
                        .text_color(bright())
                        .child(format!("Waiting for {}…", provider.entry.name)),
                )
                .child(
                    div()
                        .text_center()
                        .text_size(px(8.0))
                        .line_height(relative(1.5))
                        .text_color(faint())
                        .child("Preparing a secure browser sign-in with the provider."),
                )
                .into_any_element(),
            AuthPhase::DeviceCode {
                user_code,
                verification_uri,
            } => {
                let url = verification_uri.clone();
                div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .text_size(px(9.0))
                            .line_height(relative(1.6))
                            .text_color(dim())
                            .child(
                                "Open the provider's sign-in page and enter this code. \
                                 This window will update when the provider confirms you.",
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .border_1()
                            .border_color(line())
                            .bg(bg())
                            .p_4()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_size(px(8.0))
                                            .text_color(faint())
                                            .child("DEVICE CODE"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(18.0))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(bright())
                                            .child(user_code.clone()),
                                    ),
                            )
                            .child(
                                div()
                                    .id("open-provider-sign-in")
                                    .cursor_pointer()
                                    .text_size(px(8.0))
                                    .text_color(green())
                                    .hover(|this| this.text_color(bright()))
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.open_url(&url);
                                    }))
                                    .child("OPEN SIGN-IN PAGE  ↗"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(8.0))
                            .text_color(faint())
                            .child(Spinner::new().xsmall().color(green()))
                            .child("WAITING FOR PROVIDER CONFIRMATION…"),
                    )
                    .into_any_element()
            }
            AuthPhase::Complete { detail } => div()
                .flex()
                .flex_col()
                .items_center()
                .gap_4()
                .py_4()
                .child(
                    div()
                        .flex()
                        .size(px(42.0))
                        .items_center()
                        .justify_center()
                        .border_1()
                        .border_color(green())
                        .text_size(px(20.0))
                        .text_color(green())
                        .child("✓"),
                )
                .child(
                    div()
                        .font_family(DISPLAY_FONT)
                        .text_size(px(24.0))
                        .text_color(bright())
                        .child("Provider configured."),
                )
                .child(
                    div()
                        .text_center()
                        .text_size(px(8.0))
                        .line_height(relative(1.5))
                        .text_color(faint())
                        .child(detail.clone()),
                )
                .child(
                    div()
                        .id("finish-provider-auth")
                        .flex()
                        .h(px(42.0))
                        .w_full()
                        .cursor_pointer()
                        .items_center()
                        .justify_center()
                        .bg(green())
                        .text_size(px(9.0))
                        .font_weight(FontWeight::BOLD)
                        .text_color(bg())
                        .hover(|this| this.bg(green().opacity(0.82)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.finish_provider_auth(provider_index, window, cx);
                        }))
                        .child("RETURN TO SETTINGS →"),
                )
                .into_any_element(),
            AuthPhase::Failed(message) => div()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .border_1()
                        .border_color(red())
                        .bg(red().opacity(0.08))
                        .p_4()
                        .text_size(px(9.0))
                        .line_height(relative(1.5))
                        .text_color(bright())
                        .child(message.clone()),
                )
                .child(
                    div()
                        .id("retry-provider-auth")
                        .flex()
                        .h(px(42.0))
                        .cursor_pointer()
                        .items_center()
                        .justify_center()
                        .border_1()
                        .border_color(green())
                        .text_size(px(9.0))
                        .font_weight(FontWeight::BOLD)
                        .text_color(green())
                        .hover(|this| this.bg(green().opacity(0.08)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.auth_phase = AuthPhase::Ready;
                            cx.notify();
                        }))
                        .child("TRY AGAIN"),
                )
                .into_any_element(),
        };

        let panel = div()
            .flex()
            .w(px(520.0))
            .flex_col()
            .border_1()
            .border_color(line())
            .bg(panel())
            .shadow_lg()
            .child(
                div()
                    .flex()
                    .h(px(50.0))
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(line())
                    .px_5()
                    .text_size(px(10.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(bright())
                    .child("PROVIDER AUTHENTICATION")
                    .child(
                        div()
                            .id("cancel-provider-auth")
                            .cursor_pointer()
                            .text_color(faint())
                            .hover(|this| this.text_color(bright()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.cancel_provider_auth(cx);
                            }))
                            .child("×"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .p_6()
                    .child(
                        div()
                            .text_size(px(8.0))
                            .text_color(green())
                            .child(auth_hint.eyebrow),
                    )
                    .child(
                        div()
                            .font_family(DISPLAY_FONT)
                            .text_size(px(30.0))
                            .text_color(bright())
                            .child(format!("Connect {}", provider.entry.name)),
                    )
                    .child(phase_content),
            );

        overlay_backdrop(panel)
    }

    fn render_overlay(
        &self,
        overlay_host: OverlayHost,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        match self.overlay.visible_for(overlay_host)? {
            Overlay::None => None,
            Overlay::ProviderPicker => Some(self.render_provider_picker(overlay_host, cx)),
            Overlay::Authenticate { provider_index } => {
                Some(self.render_authentication(provider_index, cx))
            }
            Overlay::ConflictResolver => Some(self.render_conflict_resolver(cx)),
        }
    }

    fn settings_window_content(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let content = self.settings_component(cx);
        let overlay = self.render_overlay(OverlayHost::Settings, cx);

        div()
            .relative()
            .size_full()
            .child(content)
            .children(overlay)
            .into_any_element()
    }
}

impl Render for ProjectManager {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.route {
            Route::Library => self.render_library(cx),
            Route::Onboarding => self.render_onboarding(cx),
        };

        let overlay = self.render_overlay(OverlayHost::Main, cx);

        div()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::close_window))
            .on_action(cx.listener(Self::minimize_window))
            .on_action(cx.listener(Self::zoom_window))
            .on_action(cx.listener(Self::open_settings_action))
            .on_action(cx.listener(Self::add_ableton_project))
            .on_action(cx.listener(Self::add_flstudio_project))
            .on_action(cx.listener(Self::add_dawproject))
            .on_action(cx.listener(Self::add_auru_project))
            .on_action(cx.listener(Self::sort_by_last_modified_local))
            .on_action(cx.listener(Self::sort_by_last_modified_remote))
            .on_action(cx.listener(Self::sort_by_name))
            .on_action(cx.listener(Self::sort_by_recently_added))
            .on_action(cx.listener(Self::sort_by_attention_required))
            .relative()
            .size_full()
            .font_family(MONO_FONT)
            .bg(bg())
            .text_color(ink())
            .child(content)
            .children(overlay)
    }
}

fn overlay_backdrop(panel: impl IntoElement) -> AnyElement {
    div()
        .absolute()
        .top_0()
        .left_0()
        .flex()
        .size_full()
        .items_center()
        .justify_center()
        .bg(rgb(0x050706).opacity(0.78))
        .child(panel)
        .into_any_element()
}

/// The "add a project" control.
///
/// A menu rather than a single button because Auru is not an Ableton tool —
/// each DAW gets its own line, and adding another means adding one entry here
/// and one [`ImportKind`] variant. Each line dispatches its own action, so the
/// same choices work from the File menu and from a keyboard shortcut without
/// duplicating the logic.
fn add_project_menu() -> impl IntoElement {
    BarLink::new("add-project", "＋ ADD A PROJECT").dropdown_menu_with_anchor(
        Anchor::BottomLeft,
        |menu, _, _| {
            let mut menu = menu.label("Add a project from this computer");
            for kind in ImportKind::ALL {
                menu = menu.menu(kind.label(), import_action(kind));
            }
            menu
        },
    )
}

/// The sidebar's sort control.
///
/// Anchored to the top so it opens downward over the list rather than upward
/// off the top of the window — the opposite of the Add-a-project menu, which
/// sits at the bottom of the sidebar.
fn sort_menu(current: SortOrder) -> impl IntoElement {
    BarLink::new("sort-order", format!("SORT: {} ▾", current.short_label()))
        .color(green())
        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
            let mut menu = menu.label("Sort projects by");
            for order in SortOrder::ALL {
                // Checked rather than merely highlighted: with five orders that
                // all produce a plausible-looking list, the current one has to
                // be readable at a glance.
                menu = menu.menu_with_check(order.label(), order == current, sort_action(order));
            }
            menu
        })
}

fn sort_action(order: SortOrder) -> Box<dyn gpui::Action> {
    match order {
        SortOrder::LastModifiedLocal => Box::new(SortByLastModifiedLocal),
        SortOrder::LastModifiedRemote => Box::new(SortByLastModifiedRemote),
        SortOrder::NameAscending => Box::new(SortByName),
        SortOrder::RecentlyAdded => Box::new(SortByRecentlyAdded),
        SortOrder::AttentionRequired => Box::new(SortByAttentionRequired),
    }
}

/// A text link styled like the rest of the sidebar's bottom bar.
///
/// The bar's items are 9px mono in the app's own palette. A `Button` cannot be
/// made to match: it sets its own text colour and size inside `render`, so
/// anything applied from outside loses.
///
/// Built on a *stateful* div, which is not optional. An element with no id has
/// no identity between frames, so it gets no hover tracking, no cursor, and
/// nothing for a dropdown to anchor its state to — the link renders, and every
/// interaction silently does nothing. Every trigger the component library
/// ships is stateful for the same reason.
#[derive(IntoElement)]
struct BarLink {
    base: Stateful<Div>,
    label: SharedString,
    selected: bool,
    color: Option<Hsla>,
}

impl BarLink {
    fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            base: div().id(id),
            label: label.into(),
            selected: false,
            color: None,
        }
    }

    /// Override the resting colour, for bars that are not the bottom one.
    fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

impl Selectable for BarLink {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

// Both required by `DropdownMenu`, which reads the trigger's element id and
// style off them. Delegating to the inner div is the standard shape.
impl Styled for BarLink {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl InteractiveElement for BarLink {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl DropdownMenu for BarLink {}

impl RenderOnce for BarLink {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let color = self.color;
        let selected = self.selected;
        self.base
            .cursor_pointer()
            .text_size(px(9.0))
            // Reads as active while its menu is open, the same way the row it
            // sits next to brightens on hover.
            .text_color(if selected {
                bright()
            } else {
                color.unwrap_or_else(faint)
            })
            .hover(|this| this.text_color(bright()))
            .child(self.label)
    }
}

/// The action a menu line dispatches.
///
/// Kept here rather than on [`ImportKind`] so the model stays free of UI
/// types; the mapping is the one place the two vocabularies meet.
fn import_action(kind: ImportKind) -> Box<dyn gpui::Action> {
    match kind {
        ImportKind::AbletonLiveSet => Box::new(AddAbletonProject),
        ImportKind::FlStudio => Box::new(AddFlStudioProject),
        ImportKind::Dawproject => Box::new(AddDawproject),
        ImportKind::AuruProject => Box::new(AddAuruProject),
    }
}

fn info_cell(label: &'static str, value: impl IntoElement, value_color: Hsla) -> AnyElement {
    div()
        .flex()
        .min_h(px(62.0))
        .flex_1()
        .flex_col()
        .justify_center()
        .gap_2()
        .border_r_1()
        .border_color(line())
        .px_4()
        .child(div().text_size(px(8.0)).text_color(faint()).child(label))
        .child(
            div()
                .text_size(px(10.0))
                .text_color(value_color)
                .child(value),
        )
        .into_any_element()
}

fn section_label(label: &'static str) -> AnyElement {
    div()
        .pb_2()
        .text_size(px(9.0))
        .text_color(green())
        .child(label)
        .into_any_element()
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    fn parse(args: &[&str]) -> Startup {
        Options::from_args(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn settings_provider_overlays_should_only_render_in_settings() {
        let mut overlay = OverlayState::default();
        overlay.show(OverlayHost::Settings, Overlay::ProviderPicker);

        assert_eq!(
            overlay.visible_for(OverlayHost::Settings),
            Some(Overlay::ProviderPicker)
        );
        assert_eq!(overlay.visible_for(OverlayHost::Main), None);
    }

    #[test]
    fn provider_authentication_should_stay_with_the_window_that_opened_it() {
        let mut overlay = OverlayState::default();
        overlay.show(OverlayHost::Settings, Overlay::ProviderPicker);
        overlay.replace(Overlay::Authenticate { provider_index: 2 });

        assert_eq!(
            overlay.visible_for(OverlayHost::Settings),
            Some(Overlay::Authenticate { provider_index: 2 })
        );
        assert_eq!(overlay.visible_for(OverlayHost::Main), None);

        overlay.replace(Overlay::ProviderPicker);
        assert_eq!(
            overlay.visible_for(OverlayHost::Settings),
            Some(Overlay::ProviderPicker)
        );
    }

    #[test]
    fn clearing_an_overlay_should_hide_it_from_its_window() {
        let mut overlay = OverlayState::default();
        overlay.show(OverlayHost::Main, Overlay::ConflictResolver);
        overlay.clear();

        assert_eq!(overlay.visible_for(OverlayHost::Main), None);
        assert_eq!(overlay.visible_for(OverlayHost::Settings), None);
    }

    #[test]
    fn a_providers_file_should_be_accepted_in_both_spellings() {
        for args in [
            vec!["--providers-file", "providers.json"],
            vec!["--providers-file=providers.json"],
        ] {
            let Startup::Run(options) = parse(&args) else {
                panic!("{args:?} should parse");
            };
            assert_eq!(
                options.providers_file.as_deref(),
                Some(std::path::Path::new("providers.json"))
            );
        }
    }

    #[test]
    fn no_arguments_should_run_normally() {
        let Startup::Run(options) = parse(&[]) else {
            panic!("no arguments is the ordinary case");
        };
        assert!(options.providers_file.is_none());
    }

    #[test]
    fn asking_for_help_should_not_be_an_error() {
        assert!(matches!(parse(&["--help"]), Startup::ShowUsage));
        assert!(matches!(parse(&["-h"]), Startup::ShowUsage));
    }

    #[test]
    fn a_misused_flag_should_say_what_is_wrong() {
        let Startup::Invalid(message) = parse(&["--providers-file"]) else {
            panic!("a flag with no value is a mistake");
        };
        assert!(message.contains("needs a path"), "{message}");

        let Startup::Invalid(message) = parse(&["--wat"]) else {
            panic!("an unknown flag is a mistake");
        };
        assert!(message.contains("--wat"), "{message}");
    }

    /// Relative luminance, for checking one tone reads against another.
    fn luminance(color: Hsla) -> f32 {
        let rgba = gpui::Rgba::from(color);
        let channel = |c: f32| {
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(rgba.r) + 0.7152 * channel(rgba.g) + 0.0722 * channel(rgba.b)
    }

    fn contrast(a: Hsla, b: Hsla) -> f32 {
        let (x, y) = (luminance(a), luminance(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn the_hover_tone_should_be_visible_against_a_menu() {
        // Menus draw on `panel`. The previous accent sat at about 1.06:1
        // against it, which is no hover at all.
        for appearance in [Appearance::Night, Appearance::Day] {
            appearance.set();
            let seen = contrast(panel(), hover());
            assert!(
                seen > 1.2,
                "{appearance:?} hover is only {seen:.2}:1 against the menu background"
            );
        }
        Appearance::Night.set();
    }

    #[test]
    fn hover_should_be_a_bigger_step_than_selection() {
        // Selection marks the row you chose and can be quiet; hover has to
        // register the moment the pointer lands.
        for appearance in [Appearance::Night, Appearance::Day] {
            appearance.set();
            assert!(
                contrast(panel(), hover()) > contrast(panel(), selection()),
                "{appearance:?} hover should read more strongly than selection"
            );
        }
        Appearance::Night.set();
    }

    #[test]
    fn the_bar_link_should_track_its_open_state() {
        // A popover trigger has to report and accept selection; the menu uses
        // it to show the link as active while the menu is open.
        let link = BarLink::new("add-project", "＋ ADD A PROJECT");
        assert!(!link.is_selected());
        assert!(link.selected(true).is_selected());
    }

    #[test]
    fn appearance_should_round_trip_through_its_key() {
        for option in [Appearance::Night, Appearance::Day] {
            assert_eq!(Appearance::from_key(option.key()), option);
            assert!(!option.label().is_empty());
        }
        // An unreadable value must not leave the app in a half-applied state.
        assert_eq!(Appearance::from_key("chartreuse"), Appearance::Night);
    }

    #[test]
    fn each_appearance_should_map_to_a_component_theme() {
        // Both halves have to move together, so every appearance needs a
        // theme mode to hand gpui-component.
        assert_eq!(Appearance::Night.theme_mode(), ThemeMode::Dark);
        assert_eq!(Appearance::Day.theme_mode(), ThemeMode::Light);
    }

    #[test]
    fn the_palette_should_change_with_the_appearance() {
        // The bug this fixes: the app drew from fixed colours, so switching
        // theme restyled only the component widgets.
        Appearance::Night.set();
        let night = (bg(), ink(), green());
        Appearance::Day.set();
        let day = (bg(), ink(), green());
        Appearance::Night.set();

        assert_ne!(night.0, day.0, "background should differ");
        assert_ne!(night.1, day.1, "text should differ");
        assert_ne!(
            night.2, day.2,
            "status colours are re-picked for contrast, not reused"
        );
    }

    #[test]
    fn retention_options_should_round_trip_through_their_keys() {
        // The dropdown stores a key and reads it back; a key that does not
        // round-trip would silently reset the setting on every render.
        for option in VersionRetention::ALL {
            assert_eq!(VersionRetention::from_key(option.key()), option);
            assert!(!option.label().is_empty());
        }
    }

    #[test]
    fn an_unknown_retention_key_should_fall_back_to_keeping_everything() {
        // Losing history because a stored value was not understood would be
        // the worst possible way to be wrong about this.
        assert_eq!(
            VersionRetention::from_key("something-else"),
            VersionRetention::Everything
        );
    }

    #[test]
    fn retention_options_should_translate_to_provider_rules() {
        assert_eq!(VersionRetention::Everything.rule_at(2_000_000_000), None);
        assert_eq!(
            VersionRetention::LastFifty.rule_at(2_000_000_000),
            Some(auru_pm::RetentionRule::Latest { count: 50 })
        );
        assert_eq!(
            VersionRetention::LastYear.rule_at(2_000_000_000),
            Some(auru_pm::RetentionRule::Since {
                timestamp: 1_968_464_000
            })
        );
    }

    #[test]
    fn retention_keys_should_be_distinct() {
        let mut keys: Vec<&str> = VersionRetention::ALL.iter().map(|o| o.key()).collect();
        keys.sort_unstable();
        let count = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), count, "two options share a key");
    }

    #[test]
    fn an_automatic_backup_should_defer_a_revision_saved_during_preparation() {
        let qualified_revision = UNIX_EPOCH + Duration::from_secs(300);
        let saved_during_preparation = qualified_revision + Duration::from_secs(1);
        let start = BackupStart::AfterQuietPeriod { qualified_revision };

        assert!(start.accepts_prepared_revision(Some(qualified_revision)));
        assert!(!start.accepts_prepared_revision(Some(saved_during_preparation)));
        assert!(
            BackupStart::Immediate.accepts_prepared_revision(Some(saved_during_preparation)),
            "manual backups should still capture the latest stable save immediately"
        );
    }

    #[test]
    fn onboarding_should_move_through_profile_provider_and_music_steps() {
        let mut step = OnboardingStep::Profile;
        assert_eq!(step.position(), (1, 3));
        step = step.next().expect("provider step");
        assert_eq!(step, OnboardingStep::Provider);
        step = step.next().expect("music step");
        assert_eq!(step, OnboardingStep::Music);
        assert!(step.next().is_none());
        assert_eq!(step.previous(), Some(OnboardingStep::Provider));
    }

    #[test]
    fn usage_should_document_every_flag_that_exists() {
        assert!(USAGE.contains("--providers-file"));
        assert!(USAGE.contains("--help"));
    }
}

fn status_color(status: ProjectStatus) -> Hsla {
    match status {
        // Never backed up is the state most worth acting on, so it takes the
        // same warning colour as work waiting to be copied.
        ProjectStatus::NeverBackedUp => amber(),
        ProjectStatus::NotDownloaded => faint(),
        ProjectStatus::Downloaded => green(),
        ProjectStatus::Syncing => blue(),
        ProjectStatus::OutOfSync(SyncDirection::LocalAhead) => amber(),
        ProjectStatus::OutOfSync(SyncDirection::UpstreamAhead) => blue(),
        ProjectStatus::Conflicted => red(),
    }
}

fn conflict_value_label(value: &Option<serde_json::Value>) -> String {
    let Some(value) = value else {
        return "Not present".to_owned();
    };
    match value {
        serde_json::Value::Null => "None".to_owned(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::String(value) => {
            const MAX_CHARS: usize = 160;
            let mut label = value.chars().take(MAX_CHARS).collect::<String>();
            if value.chars().count() > MAX_CHARS {
                label.push('…');
            }
            label
        }
        serde_json::Value::Array(values) => format!("{} item(s)", values.len()),
        serde_json::Value::Object(fields) => format!("{} field(s)", fields.len()),
    }
}

/// Which palette the app draws with.
///
/// A process-wide atomic rather than a gpui global because the palette
/// functions below are called from inside element builders that have no `App`
/// handle — threading one through every call site would be a large change to
/// say something that is genuinely app-wide and single-valued.
static APPEARANCE: AtomicU8 = AtomicU8::new(Appearance::NIGHT);

/// The app's own colours.
///
/// gpui-component styles its widgets from its own [`Theme`]; everything Auru
/// draws by hand uses these. Both are switched together by
/// [`ProjectManager::apply_appearance`], because a theme that only reached
/// half the window would be worse than one that did not move at all.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Appearance {
    Night,
    Day,
}

impl Appearance {
    const NIGHT: u8 = 0;
    const DAY: u8 = 1;

    fn current() -> Self {
        match APPEARANCE.load(Ordering::Relaxed) {
            Self::DAY => Self::Day,
            _ => Self::Night,
        }
    }

    fn set(self) {
        APPEARANCE.store(
            match self {
                Self::Night => Self::NIGHT,
                Self::Day => Self::DAY,
            },
            Ordering::Relaxed,
        );
    }

    const fn theme_mode(self) -> ThemeMode {
        match self {
            Self::Night => ThemeMode::Dark,
            Self::Day => ThemeMode::Light,
        }
    }

    const fn key(self) -> &'static str {
        match self {
            Self::Night => "night",
            Self::Day => "day",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Night => "Night",
            Self::Day => "Day",
        }
    }

    fn from_key(key: &str) -> Self {
        if key == Self::Day.key() {
            Self::Day
        } else {
            Self::Night
        }
    }
}

/// Point gpui-component's theme at Auru's palette.
///
/// Its widgets style themselves from this theme, so left on stock values they
/// read as a different application sitting inside this one. The accent matters
/// most: it is what a menu or list row paints on hover, and the default is far
/// too close to Auru's background to register as a hover at all.
///
/// Called after every [`Theme::change`], which resets these to the built-in
/// palette for the new mode.
fn tint_component_theme(cx: &mut App) {
    let theme = Theme::global_mut(cx);
    theme.accent = hover();
    theme.accent_foreground = bright();
    theme.background = bg();
    theme.foreground = ink();
    theme.border = line();
    theme.popover = panel();
    theme.popover_foreground = ink();
    theme.muted = panel();
    theme.muted_foreground = dim();

    // The sidebar scrollbar overlays the list, so the track has to disappear
    // into the background rather than draw a channel down the rows. The thumb
    // is the same weight as a border at rest and lifts under the pointer.
    theme.scrollbar = bg().opacity(0.0);
    theme.scrollbar_thumb = faint();
    theme.scrollbar_thumb_hover = dim();

    // `tokens` is derived from `colors`, so it has to be rebuilt for the new
    // values to reach the components that read it — the menu hover among them.
    theme.tokens = ThemeTokens::from(&theme.colors);
}

/// Pick the colour for the palette in force.
fn tone(night: u32, day: u32) -> Hsla {
    match Appearance::current() {
        Appearance::Night => rgb(night).into(),
        Appearance::Day => rgb(day).into(),
    }
}

fn bg() -> Hsla {
    tone(0x0f1211, 0xf6f7f5)
}

fn panel() -> Hsla {
    tone(0x121614, 0xffffff)
}

fn selection() -> Hsla {
    tone(0x161c19, 0xe7ebe7)
}

/// Background for the row the pointer is over.
///
/// Deliberately a bigger step than [`selection`]: that marks the row you chose
/// and can afford to be quiet, whereas this has to be obvious the moment the
/// pointer lands. Menus draw on [`panel`], so it is pitched against that
/// rather than against [`bg`].
fn hover() -> Hsla {
    tone(0x27302b, 0xdbe2dc)
}

fn line() -> Hsla {
    tone(0x1e2422, 0xd7dcd8)
}

fn ink() -> Hsla {
    tone(0xd9ddda, 0x2a302c)
}

fn bright() -> Hsla {
    tone(0xeef1ee, 0x11150f)
}

fn dim() -> Hsla {
    tone(0x8a918c, 0x5f665f)
}

fn faint() -> Hsla {
    tone(0x5c645f, 0x8b918b)
}

// Status colours are darkened for the light palette rather than reused: the
// same mint green that reads clearly on near-black is nearly invisible on
// near-white, and status is the one thing here that must stay legible.
fn green() -> Hsla {
    tone(0x8fd3a8, 0x2e7d51)
}

fn amber() -> Hsla {
    tone(0xe0b064, 0x9a6b14)
}

fn blue() -> Hsla {
    tone(0x8ab5d8, 0x2d6489)
}

fn red() -> Hsla {
    tone(0xd96a55, 0xb03726)
}

fn waveform() -> Hsla {
    tone(0x39423d, 0xc3cac4)
}

/// The settings window's contents.
///
/// A separate window rather than an overlay: settings are a place you go and
/// come back from, and the OS already knows how to title, move, resize and
/// close a window. Reimplementing that inside the main window meant a fixed
/// panel that could not be resized while the settings component wanted room.
///
/// It holds the same [`ProjectManager`] the main window does, so a change made
/// here is a change to the one piece of state both windows read.
struct SettingsWindow {
    manager: Entity<ProjectManager>,
}

impl SettingsWindow {
    fn new(manager: Entity<ProjectManager>) -> Self {
        Self { manager }
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let manager = self.manager.clone();
        div()
            .size_full()
            .font_family(MONO_FONT)
            .bg(bg())
            .text_color(ink())
            .child(manager.update(cx, |manager, cx| manager.settings_window_content(cx)))
    }
}

/// Open the settings window, or focus it if it is already open.
fn open_settings_window(manager: Entity<ProjectManager>, cx: &mut App) {
    if let Some(handle) = manager.read(cx).settings_window {
        // Already open — bring it forward rather than stacking another.
        let activated = cx
            .update_window(handle.into(), |_, window, _| window.activate_window())
            .is_ok();
        if activated {
            return;
        }
    }

    let opened = cx.open_window(
        WindowOptions {
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("Auru PM — Settings".into()),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Default::default(),
                size: Size {
                    width: px(860.0),
                    height: px(620.0),
                },
            })),
            window_min_size: Some(Size {
                width: px(620.0),
                height: px(420.0),
            }),
            app_id: Some("studio.auru.pm".into()),
            ..Default::default()
        },
        {
            let manager = manager.clone();
            move |window, cx| {
                window.activate_window();
                let view = cx.new(|_| SettingsWindow::new(manager.clone()));
                cx.new(|cx| Root::new(view, window, cx))
            }
        },
    );

    match opened {
        Ok(handle) => {
            manager.update(cx, |manager, _| {
                manager.settings_window = Some(handle);
            });
        }
        Err(error) => eprintln!("[auru-pm] could not open the settings window: {error}"),
    }
}

/// Options the app was started with.
#[derive(Clone, Debug, Default)]
struct Options {
    /// A provider list to use instead of the hosted registry.
    providers_file: Option<PathBuf>,
}

impl Options {
    /// Parse the command line.
    ///
    /// Hand-rolled rather than pulling in an argument parser for one flag; if
    /// a second one arrives, that is the moment to reach for a crate.
    fn from_args(args: impl IntoIterator<Item = String>) -> Startup {
        let mut options = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--help" | "-h" => return Startup::ShowUsage,
                "--providers-file" => {
                    let Some(value) = args.next() else {
                        return Startup::Invalid(
                            "--providers-file needs a path, eg --providers-file providers.json"
                                .to_owned(),
                        );
                    };
                    options.providers_file = Some(PathBuf::from(value));
                }
                other if other.starts_with("--providers-file=") => {
                    let (_, value) = other.split_once('=').expect("checked above");
                    options.providers_file = Some(PathBuf::from(value));
                }
                other => return Startup::Invalid(format!("unknown option {other}")),
            }
        }
        Startup::Run(options)
    }
}

/// What the command line asked for.
#[derive(Debug)]
enum Startup {
    Run(Options),
    /// `--help`. A request that was understood and answered, not a failure.
    ShowUsage,
    Invalid(String),
}

const USAGE: &str = "\
auru-pm-ui — Auru project management

    --providers-file <path>   Load the provider list from a JSON file instead
                              of the hosted registry. See
                              providers.example.json for the format.
    -h, --help                Show this message.";

fn main() {
    let options = match Options::from_args(std::env::args().skip(1)) {
        Startup::Run(options) => options,
        Startup::ShowUsage => {
            println!("{USAGE}");
            return;
        }
        Startup::Invalid(message) => {
            eprintln!("{message}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    application()
        .with_assets(gpui_component_assets::Assets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            menus::init(cx);
            cx.activate(true);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let result = cx.open_window(
                WindowOptions {
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("Auru PM".into()),
                        ..Default::default()
                    }),
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: Size {
                            width: px(1_180.0),
                            height: px(740.0),
                        },
                    })),
                    window_min_size: Some(Size {
                        width: px(820.0),
                        height: px(560.0),
                    }),
                    app_id: Some("studio.auru.pm".into()),
                    ..Default::default()
                },
                |window, cx| {
                    window.activate_window();
                    Theme::change(Appearance::current().theme_mode(), Some(window), cx);
                    tint_component_theme(cx);
                    let view: Entity<ProjectManager> =
                        cx.new(|cx| ProjectManager::new(options.clone(), window, cx));
                    view.update(cx, |manager, cx| {
                        manager.refresh_remote_projects(cx);
                        manager.start_automatic_backup_watcher(window, cx);
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                },
            );
            if let Err(error) = result {
                eprintln!("failed to open Auru PM window: {error}");
                cx.quit();
            }
        });
}

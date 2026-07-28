mod catalog;
mod menus;
mod model;

use std::time::Duration;

use auru_pm::{AURU_REGISTRY_URL, AuthMethod};
use gpui::{
    AnyElement, App, Bounds, Context, Entity, FocusHandle, FontWeight, Hsla, IntoElement,
    ParentElement, Render, Size, Styled, Subscription, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, relative, rgb,
};
use gpui_component::{
    Root, Sizable, Theme, ThemeMode, WindowExt,
    input::{Input, InputEvent, InputState},
    notification::Notification,
    scroll::ScrollableElement,
    spinner::Spinner,
};
use gpui_platform::application;

use crate::catalog::{
    CatalogState, ProviderAvailability, ProviderListing, fetch_first_party_catalog,
    stub_provider_catalog,
};
use crate::menus::{CloseWindow, Minimize, OpenSettings, Zoom};
use crate::model::{
    PLUGIN_SETTINGS_REASSURANCE, Project, ProjectAction, ProjectStatus, SyncDirection,
    stub_projects,
};

const TRANSFER_DURATION: Duration = Duration::from_millis(1_500);
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
    Recovery,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProviderEntryPoint {
    Settings,
    Recovery,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Overlay {
    None,
    Settings,
    ProviderPicker,
    Authenticate {
        provider_index: usize,
        entry_point: ProviderEntryPoint,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthPhase {
    Ready,
    Waiting,
    Complete,
}

struct ProjectManager {
    focus_handle: FocusHandle,
    projects: Vec<Project>,
    selected_project: usize,
    route: Route,
    overlay: Overlay,
    display_name: String,
    display_name_input: Entity<InputState>,
    search_input: Entity<InputState>,
    credential_input: Entity<InputState>,
    providers: Vec<ProviderListing>,
    catalog_state: CatalogState,
    auth_phase: AuthPhase,
    automatic_backups: bool,
    verify_uploads: bool,
    _subscriptions: Vec<Subscription>,
}

impl ProjectManager {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);

        let display_name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g. Alice, Bob, or Charlie"));
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("⌕ search projects…"));
        let credential_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Personal access token"));

        let _subscriptions = [&display_name_input, &search_input, &credential_input]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |_, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                })
            })
            .collect();

        let catalog_task = cx
            .background_executor()
            .spawn(async { fetch_first_party_catalog() });
        cx.spawn(async move |this, cx| {
            let catalog = catalog_task.await;
            _ = this.update(cx, |this, cx| {
                match catalog {
                    Ok(mut providers) => {
                        for provider in &mut providers {
                            if this.providers.iter().any(|current| {
                                current.entry.id == provider.entry.id
                                    && current.availability == ProviderAvailability::Connected
                            }) {
                                provider.mark_connected();
                            }
                        }
                        this.providers = providers;
                        this.catalog_state = CatalogState::Live;
                    }
                    Err(_) => this.catalog_state = CatalogState::Fallback,
                }
                cx.notify();
            });
        })
        .detach();

        Self {
            focus_handle,
            projects: stub_projects(),
            selected_project: 0,
            route: Route::Library,
            overlay: Overlay::None,
            display_name: "Alice".to_owned(),
            display_name_input,
            search_input,
            credential_input,
            providers: stub_provider_catalog(),
            catalog_state: CatalogState::Loading,
            auth_phase: AuthPhase::Ready,
            automatic_backups: true,
            verify_uploads: true,
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

    fn open_settings_action(&mut self, _: &OpenSettings, _: &mut Window, cx: &mut Context<Self>) {
        self.route = Route::Library;
        self.overlay = Overlay::Settings;
        cx.notify();
    }

    fn handle_project_action(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.projects.get(index) else {
            return;
        };
        let action = project.status.action();
        let project_name = project.name;

        match action {
            ProjectAction::Download | ProjectAction::Push | ProjectAction::Pull => {
                let Some(project) = self.projects.get_mut(index) else {
                    return;
                };
                if !project.begin_transfer() {
                    return;
                }
                cx.notify();

                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(TRANSFER_DURATION).await;
                    _ = this.update(cx, |this, cx| {
                        if let Some(project) = this.projects.get_mut(index) {
                            project.finish_transfer();
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            ProjectAction::Open => {
                window.push_notification(
                    Notification::info("The native DAW launch hook will replace this demo action.")
                        .title(format!("{project_name} is ready")),
                    cx,
                );
            }
            ProjectAction::ReviewConflicts => {
                window.push_notification(
                    Notification::warning(
                        "Both versions are preserved. The compare view is the next integration point.",
                    )
                    .title(format!("{project_name} needs a decision")),
                    cx,
                );
            }
            ProjectAction::None => {}
        }
    }

    fn back_up_all(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let mut syncing = Vec::new();
        for (index, project) in self.projects.iter_mut().enumerate() {
            if project.status.action() == ProjectAction::Push && project.begin_transfer() {
                syncing.push(index);
            }
        }

        if syncing.is_empty() {
            return;
        }
        cx.notify();

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TRANSFER_DURATION).await;
            _ = this.update(cx, |this, cx| {
                for index in syncing {
                    if let Some(project) = this.projects.get_mut(index) {
                        project.finish_transfer();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn complete_onboarding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        self.route = Route::Library;
        window.push_notification(
            Notification::success("Your local profile is ready. No account was created.")
                .title("Welcome to Auru PM"),
            cx,
        );
        cx.notify();
    }

    fn select_provider(
        &mut self,
        provider_index: usize,
        entry_point: ProviderEntryPoint,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(provider) = self.providers.get(provider_index) else {
            return;
        };

        if provider.requires_authentication() {
            self.credential_input
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.auth_phase = AuthPhase::Ready;
            self.overlay = Overlay::Authenticate {
                provider_index,
                entry_point,
            };
        } else {
            let provider_name = provider.entry.name.clone();
            if let Some(provider) = self.providers.get_mut(provider_index) {
                provider.mark_connected();
            }
            self.overlay = match entry_point {
                ProviderEntryPoint::Settings => Overlay::Settings,
                ProviderEntryPoint::Recovery => Overlay::None,
            };
            if entry_point == ProviderEntryPoint::Recovery {
                self.route = Route::Library;
            }
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

        if provider.preferred_auth_method() == AuthMethod::Pat
            && self.credential_input.read(cx).value().trim().is_empty()
        {
            window.push_notification(
                Notification::warning("Enter the token issued by this provider.")
                    .title("Credential needed"),
                cx,
            );
            return;
        }

        self.auth_phase = AuthPhase::Waiting;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TRANSFER_DURATION).await;
            _ = this.update(cx, |this, cx| {
                this.auth_phase = AuthPhase::Complete;
                cx.notify();
            });
        })
        .detach();
    }

    fn finish_provider_auth(
        &mut self,
        provider_index: usize,
        entry_point: ProviderEntryPoint,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(provider) = self.providers.get_mut(provider_index) else {
            return;
        };
        provider.mark_connected();
        let provider_name = provider.entry.name.clone();

        match entry_point {
            ProviderEntryPoint::Settings => self.overlay = Overlay::Settings,
            ProviderEntryPoint::Recovery => {
                self.overlay = Overlay::None;
                self.route = Route::Library;
            }
        }
        self.auth_phase = AuthPhase::Ready;
        window.push_notification(
            Notification::success("The provider confirmed this device.")
                .title(format!("Signed in to {provider_name}")),
            cx,
        );
        cx.notify();
    }

    fn cancel_provider_auth(&mut self, entry_point: ProviderEntryPoint, cx: &mut Context<Self>) {
        self.auth_phase = AuthPhase::Ready;
        self.overlay = match entry_point {
            ProviderEntryPoint::Settings => Overlay::ProviderPicker,
            ProviderEntryPoint::Recovery => Overlay::None,
        };
        cx.notify();
    }

    fn reset_demo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.projects = stub_projects();
        self.providers = stub_provider_catalog();
        self.catalog_state = CatalogState::Fallback;
        self.selected_project = 0;
        self.display_name = "Alice".to_owned();
        self.route = Route::Library;
        self.overlay = Overlay::None;
        self.auth_phase = AuthPhase::Ready;
        self.automatic_backups = true;
        self.verify_uploads = true;
        window.push_notification(
            Notification::info("Every simulated state is back at its starting point.")
                .title("Demo reset"),
            cx,
        );
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
                    .px_5()
                    .pb_2()
                    .pt_3()
                    .text_size(px(9.0))
                    .text_color(green())
                    .child(format!(
                        "[ LIBRARY · {} PROJECTS · {attention_count} NEED YOU ]",
                        self.projects.len()
                    )),
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
                div().min_h_0().flex_1().overflow_y_scrollbar().children(
                    self.projects
                        .iter()
                        .enumerate()
                        .filter(|(_, project)| {
                            search_query.is_empty()
                                || project.name.to_lowercase().contains(&search_query)
                                || project.file_name.to_lowercase().contains(&search_query)
                        })
                        .map(|(index, project)| self.render_project_row(index, project, cx)),
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
                    .px_5()
                    .text_size(px(9.0))
                    .text_color(faint())
                    .child(
                        div()
                            .id("add-project")
                            .cursor_pointer()
                            .hover(|this| this.text_color(bright()))
                            .on_click(cx.listener(|_, _, window, cx| {
                                window.push_notification(
                                    Notification::info(
                                        "Folder discovery will replace this demo action.",
                                    )
                                    .title("Add a project"),
                                    cx,
                                );
                            }))
                            .child("＋ ADD A PROJECT"),
                    )
                    .child(
                        div()
                            .id("open-settings")
                            .cursor_pointer()
                            .hover(|this| this.text_color(bright()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.overlay = Overlay::Settings;
                                cx.notify();
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
            .child(self.render_demo_bar(cx))
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
            .h(px(56.0))
            .cursor_pointer()
            .items_center()
            .gap_3()
            .border_1()
            .border_color(if selected { color } else { bg() })
            .bg(if selected { selection() } else { bg() })
            .px_5()
            .hover(|this| this.bg(selection()))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected_project = index;
                cx.notify();
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
                            .child(project.name),
                    )
                    .child(
                        div()
                            .overflow_x_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(9.0))
                            .text_color(color)
                            .child(project.list_status()),
                    ),
            )
            .child(div().text_size(px(13.0)).text_color(faint()).child("›"))
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
                            .child(project.name),
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
                        project.safe_version,
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
                                            .on_click(cx.listener(|_, _, window, cx| {
                                                window.push_notification(
                                            Notification::info(
                                                "Restore is simulated; no files were changed.",
                                            )
                                            .title("Version selected"),
                                            cx,
                                        );
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
                    .child("VIEW FULL HISTORY →")
                    .child(
                        div()
                            .cursor_pointer()
                            .hover(|this| this.text_color(bright()))
                            .child(project.open_label()),
                    ),
            )
            .into_any_element()
    }

    fn render_demo_bar(&self, cx: &mut Context<Self>) -> AnyElement {
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
            .child("[ DEMO ] SIMULATED DATA — TRY THE STORY:")
            .child(
                div()
                    .flex()
                    .gap_6()
                    .child(
                        div()
                            .id("demo-onboarding")
                            .cursor_pointer()
                            .hover(|this| this.text_color(bright()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.route = Route::Onboarding;
                                this.overlay = Overlay::None;
                                cx.notify();
                            }))
                            .child("▶ FIRST-RUN SETUP"),
                    )
                    .child(
                        div()
                            .id("demo-recovery")
                            .cursor_pointer()
                            .hover(|this| this.text_color(bright()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.route = Route::Recovery;
                                this.overlay = Overlay::None;
                                cx.notify();
                            }))
                            .child("⚡ RECOVERY MODE"),
                    )
                    .child(
                        div()
                            .id("demo-reset")
                            .cursor_pointer()
                            .hover(|this| this.text_color(bright()))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.reset_demo(window, cx);
                            }))
                            .child("↺ RESET"),
                    ),
            )
            .into_any_element()
    }

    fn render_onboarding(&self, cx: &mut Context<Self>) -> AnyElement {
        let can_continue = !self.display_name_input.read(cx).value().trim().is_empty();
        let mut continue_button = div()
            .id("finish-onboarding")
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
            .child("OPEN MY LIBRARY →");

        if can_continue {
            continue_button = continue_button
                .cursor_pointer()
                .hover(|this| this.bg(green().opacity(0.82)))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.complete_onboarding(window, cx);
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
                    .child("1 / 1"),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .child(
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
                                    .child("Hello. What should we call you?"),
                            )
                            .child(
                                div()
                                    .max_w(px(500.0))
                                    .text_size(px(10.0))
                                    .line_height(relative(1.6))
                                    .text_color(dim())
                                    .child(
                                        "This name appears in project history. It stays with your local profile — no login or account is created.",
                                    ),
                            )
                            .child(
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
                                    .child(Input::new(&self.display_name_input).w_full()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .id("skip-onboarding")
                                            .cursor_pointer()
                                            .text_size(px(9.0))
                                            .text_color(faint())
                                            .hover(|this| this.text_color(bright()))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.route = Route::Library;
                                                cx.notify();
                                            }))
                                            .child("← BACK TO DEMO"),
                                    )
                                    .child(continue_button),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_recovery(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .bg(bg())
            .child(
                div()
                    .flex()
                    .w(px(520.0))
                    .flex_col()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .text_size(px(9.0))
                            .text_color(faint())
                            .child("AURU PM"),
                    )
                    .child(
                        div()
                            .text_center()
                            .font_family(DISPLAY_FONT)
                            .text_size(px(40.0))
                            .line_height(relative(1.12))
                            .text_color(bright())
                            .child("Your music,\noff your hardware."),
                    )
                    .child(
                        div()
                            .mb_2()
                            .text_center()
                            .text_size(px(10.0))
                            .line_height(relative(1.6))
                            .text_color(dim())
                            .child(
                                "Choose the provider that holds your library. Auru PM signs in only when that provider asks for authentication.",
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .w_full()
                            .flex_col()
                            .gap_2()
                            .border_1()
                            .border_color(line())
                            .bg(panel())
                            .p_4()
                            .child(section_label("[ SIGN IN WITH A PROVIDER ]"))
                            .children(
                                self.providers
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, provider)| provider.requires_authentication())
                                    .map(|(index, provider)| {
                                        self.render_recovery_provider(index, provider, cx)
                                    }),
                            )
                            .child(
                                div()
                                    .pt_2()
                                    .text_size(px(8.0))
                                    .text_color(faint())
                                    .child(format!(
                                        "{} · {AURU_REGISTRY_URL}",
                                        self.catalog_state.label()
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .id("recovery-back")
                            .mt_2()
                            .cursor_pointer()
                            .text_size(px(8.0))
                            .text_color(faint())
                            .hover(|this| this.text_color(bright()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.route = Route::Library;
                                cx.notify();
                            }))
                            .child("[ DEMO ] BACK TO LIBRARY"),
                    ),
            )
            .into_any_element()
    }

    fn render_recovery_provider(
        &self,
        index: usize,
        provider: &ProviderListing,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name = provider.entry.name.clone();
        div()
            .id(format!("recovery-provider-{}", provider.entry.id))
            .flex()
            .min_h(px(58.0))
            .cursor_pointer()
            .items_center()
            .justify_between()
            .gap_4()
            .border_1()
            .border_color(line())
            .px_4()
            .hover(|this| this.border_color(green()).bg(selection()))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_provider(index, ProviderEntryPoint::Recovery, window, cx);
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .font_family(DISPLAY_FONT)
                            .text_size(px(15.0))
                            .text_color(bright())
                            .child(name),
                    )
                    .child(
                        div()
                            .text_size(px(8.0))
                            .text_color(faint())
                            .child(provider.detail.clone()),
                    ),
            )
            .child(
                div()
                    .text_size(px(8.0))
                    .text_color(green())
                    .child("SIGN IN →"),
            )
            .into_any_element()
    }

    fn render_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let panel = div()
            .flex()
            .h(relative(0.86))
            .w(px(640.0))
            .flex_col()
            .border_1()
            .border_color(line())
            .bg(panel())
            .shadow_lg()
            .child(
                div()
                    .flex()
                    .h(px(50.0))
                    .flex_shrink_0()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(line())
                    .px_5()
                    .text_size(px(10.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(bright())
                    .child("SETTINGS")
                    .child(
                        div()
                            .id("close-settings")
                            .cursor_pointer()
                            .text_color(faint())
                            .hover(|this| this.text_color(bright()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.overlay = Overlay::None;
                                cx.notify();
                            }))
                            .child("×"),
                    ),
            )
            .child(
                div()
                    .min_h_0()
                    .flex_1()
                    .overflow_y_scrollbar()
                    .p_5()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_6()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(section_label("[ WHERE BACKUPS LIVE ]"))
                                    .children(
                                        self.providers
                                            .iter()
                                            .enumerate()
                                            .map(|(index, provider)| {
                                                self.render_settings_provider(index, provider)
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
                                            .hover(|this| {
                                                this.border_color(green()).text_color(green())
                                            })
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.overlay = Overlay::ProviderPicker;
                                                cx.notify();
                                            }))
                                            .child(
                                                "＋ ADD ANOTHER PROVIDER — CURATED OR CUSTOM URL…",
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(section_label("[ BACKUP BEHAVIOUR ]"))
                                    .child(self.render_setting_toggle(
                                        "automatic-backups",
                                        "Back up automatically after changes",
                                        "Waits for five quiet minutes, then copies in the background",
                                        self.automatic_backups,
                                        cx,
                                    ))
                                    .child(self.render_setting_toggle(
                                        "verify-uploads",
                                        "Verify every copy after upload",
                                        "Re-reads stored files and checks nothing is missing or damaged",
                                        self.verify_uploads,
                                        cx,
                                    ))
                                    .child(
                                        div()
                                            .flex()
                                            .min_h(px(54.0))
                                            .items_center()
                                            .justify_between()
                                            .border_t_1()
                                            .border_color(line())
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_col()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_size(px(10.0))
                                                            .text_color(ink())
                                                            .child("Old versions"),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_size(px(8.0))
                                                            .text_color(faint())
                                                            .child("How much history to keep"),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .border_1()
                                                    .border_color(line())
                                                    .px_4()
                                                    .py_2()
                                                    .text_size(px(8.0))
                                                    .text_color(dim())
                                                    .child("KEEP EVERY VERSION  ▾"),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_3()
                                    .child(section_label("[ APPEARANCE ]"))
                                    .child(
                                        div()
                                            .flex()
                                            .gap_2()
                                            .child(theme_button("NIGHT", true))
                                            .child(theme_button("STUDIO BLACK", false))
                                            .child(theme_button("DAY", false)),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_3()
                                    .child(section_label("[ LOCAL PROFILE ]"))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .border_t_1()
                                            .border_color(line())
                                            .pt_4()
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_col()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .font_family(DISPLAY_FONT)
                                                            .text_size(px(16.0))
                                                            .text_color(bright())
                                                            .child(self.display_name.clone()),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_size(px(8.0))
                                                            .text_color(faint())
                                                            .child(
                                                                "Display name only · providers authenticate separately",
                                                            ),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .id("recover-another-device")
                                                    .cursor_pointer()
                                                    .border_1()
                                                    .border_color(line())
                                                    .px_4()
                                                    .py_2()
                                                    .text_size(px(8.0))
                                                    .text_color(dim())
                                                    .hover(|this| {
                                                        this.border_color(green())
                                                            .text_color(green())
                                                    })
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.overlay = Overlay::None;
                                                        this.route = Route::Recovery;
                                                        cx.notify();
                                                    }))
                                                    .child("RECOVER ANOTHER DEVICE"),
                                            ),
                                    ),
                            ),
                    ),
            );

        overlay_backdrop(panel)
    }

    fn render_settings_provider(&self, _index: usize, provider: &ProviderListing) -> AnyElement {
        let connected = provider.availability == ProviderAvailability::Connected;
        div()
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
                            ),
                    ),
            )
            .child(
                div()
                    .text_size(px(8.0))
                    .text_color(if connected { green() } else { faint() })
                    .child(provider.availability.label()),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_setting_toggle(
        &self,
        id: &'static str,
        title: &'static str,
        detail: &'static str,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(id)
            .flex()
            .min_h(px(54.0))
            .cursor_pointer()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(line())
            .hover(|this| this.bg(selection()))
            .on_click(cx.listener(move |this, _, _, cx| {
                if id == "automatic-backups" {
                    this.automatic_backups = !this.automatic_backups;
                } else {
                    this.verify_uploads = !this.verify_uploads;
                }
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_size(px(10.0)).text_color(ink()).child(title))
                    .child(div().text_size(px(8.0)).text_color(faint()).child(detail)),
            )
            .child(
                div()
                    .flex()
                    .h(px(16.0))
                    .w(px(34.0))
                    .items_center()
                    .justify_end()
                    .bg(if enabled {
                        green().opacity(0.35)
                    } else {
                        line()
                    })
                    .p(px(3.0))
                    .when(!enabled, |this| this.justify_start())
                    .child(
                        div()
                            .size(px(10.0))
                            .bg(if enabled { bright() } else { faint() }),
                    ),
            )
            .into_any_element()
    }

    fn render_provider_picker(&self, cx: &mut Context<Self>) -> AnyElement {
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
                        this.select_provider(index, ProviderEntryPoint::Settings, window, cx);
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
                                this.overlay = Overlay::Settings;
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
                    .child(div().flex().flex_col().gap_2().children(provider_rows))
                    .child(
                        div()
                            .id("add-custom-provider")
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
                            .on_click(cx.listener(|_, _, window, cx| {
                                window.push_notification(
                                    Notification::info(
                                        "Custom URL validation will use the provider health metadata.",
                                    )
                                    .title("Add a custom provider"),
                                    cx,
                                );
                            }))
                            .child("＋ ADD A CUSTOM PROVIDER URL"),
                    ),
            );

        overlay_backdrop(panel)
    }

    fn render_authentication(
        &self,
        provider_index: usize,
        entry_point: ProviderEntryPoint,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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

        let phase_content = match self.auth_phase {
            AuthPhase::Ready => {
                let mut content = div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .text_size(px(9.0))
                            .line_height(relative(1.6))
                            .text_color(dim())
                            .child(match auth_method {
                                AuthMethod::OAuthDeviceCode => {
                                    "This provider requests its own sign-in. Auru PM will open the provider's device flow and wait for confirmation."
                                }
                                AuthMethod::Pat => {
                                    "This provider requests a personal access token. It will be handed to the OS credential store, not saved in project files."
                                }
                                AuthMethod::None => "This provider does not require authentication.",
                            }),
                    );

                if auth_method == AuthMethod::OAuthDeviceCode {
                    content = content.child(
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
                                            .text_size(px(16.0))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(bright())
                                            .child("AURU-M7K2"),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(8.0))
                                    .text_color(green())
                                    .child("PROVIDER-HOSTED SIGN-IN"),
                            ),
                    );
                } else if auth_method == AuthMethod::Pat {
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
                            .child(match auth_method {
                                AuthMethod::OAuthDeviceCode => "BEGIN PROVIDER SIGN-IN →",
                                AuthMethod::Pat => "CONNECT SECURELY →",
                                AuthMethod::None => "CONNECT →",
                            }),
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
                        .child("The simulated provider is confirming this device."),
                )
                .into_any_element(),
            AuthPhase::Complete => div()
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
                        .child("This device is connected."),
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
                            this.finish_provider_auth(provider_index, entry_point, window, cx);
                        }))
                        .child(if entry_point == ProviderEntryPoint::Recovery {
                            "RESTORE MY LIBRARY →"
                        } else {
                            "RETURN TO SETTINGS →"
                        }),
                )
                .into_any_element(),
        };

        let panel =
            div()
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
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.cancel_provider_auth(entry_point, cx);
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
                        .child(div().text_size(px(8.0)).text_color(green()).child(
                            match auth_method {
                                AuthMethod::OAuthDeviceCode => "OAUTH DEVICE CODE",
                                AuthMethod::Pat => "PERSONAL ACCESS TOKEN",
                                AuthMethod::None => "NO AUTHENTICATION",
                            },
                        ))
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
}

impl Render for ProjectManager {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.route {
            Route::Library => self.render_library(cx),
            Route::Onboarding => self.render_onboarding(cx),
            Route::Recovery => self.render_recovery(cx),
        };

        let overlay = match self.overlay {
            Overlay::None => None,
            Overlay::Settings => Some(self.render_settings(cx)),
            Overlay::ProviderPicker => Some(self.render_provider_picker(cx)),
            Overlay::Authenticate {
                provider_index,
                entry_point,
            } => Some(self.render_authentication(provider_index, entry_point, cx)),
        };

        div()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::close_window))
            .on_action(cx.listener(Self::minimize_window))
            .on_action(cx.listener(Self::zoom_window))
            .on_action(cx.listener(Self::open_settings_action))
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

fn theme_button(label: &'static str, selected: bool) -> AnyElement {
    div()
        .flex()
        .h(px(34.0))
        .items_center()
        .border_1()
        .border_color(if selected { bright() } else { line() })
        .px_4()
        .text_size(px(8.0))
        .text_color(if selected { bright() } else { faint() })
        .child(label)
        .into_any_element()
}

fn status_color(status: ProjectStatus) -> Hsla {
    match status {
        ProjectStatus::NotDownloaded => faint(),
        ProjectStatus::Downloaded => green(),
        ProjectStatus::Syncing => blue(),
        ProjectStatus::OutOfSync(SyncDirection::LocalAhead) => amber(),
        ProjectStatus::OutOfSync(SyncDirection::UpstreamAhead) => blue(),
        ProjectStatus::Conflicted => red(),
    }
}

fn bg() -> Hsla {
    rgb(0x0f1211).into()
}

fn panel() -> Hsla {
    rgb(0x121614).into()
}

fn selection() -> Hsla {
    rgb(0x161c19).into()
}

fn line() -> Hsla {
    rgb(0x1e2422).into()
}

fn ink() -> Hsla {
    rgb(0xd9ddda).into()
}

fn bright() -> Hsla {
    rgb(0xeef1ee).into()
}

fn dim() -> Hsla {
    rgb(0x8a918c).into()
}

fn faint() -> Hsla {
    rgb(0x5c645f).into()
}

fn green() -> Hsla {
    rgb(0x8fd3a8).into()
}

fn amber() -> Hsla {
    rgb(0xe0b064).into()
}

fn blue() -> Hsla {
    rgb(0x8ab5d8).into()
}

fn red() -> Hsla {
    rgb(0xd96a55).into()
}

fn waveform() -> Hsla {
    rgb(0x39423d).into()
}

fn main() {
    application()
        .with_assets(gpui_component_assets::Assets)
        .run(|cx: &mut App| {
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
                    Theme::change(ThemeMode::Dark, Some(window), cx);
                    let view: Entity<ProjectManager> = cx.new(|cx| ProjectManager::new(window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                },
            );
            if let Err(error) = result {
                eprintln!("failed to open Auru PM window: {error}");
                cx.quit();
            }
        });
}

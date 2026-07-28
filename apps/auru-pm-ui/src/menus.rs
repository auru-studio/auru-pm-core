use gpui::{App, KeyBinding, Menu, MenuItem, NoAction, SystemMenuType, actions};

actions!(
    auru_pm_ui,
    [
        Quit,
        Hide,
        HideOthers,
        ShowAll,
        CloseWindow,
        Minimize,
        Zoom,
        OpenSettings
    ]
);

pub fn init(cx: &mut App) {
    cx.set_menus(build_menus());
    cx.bind_keys([
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-q", Quit, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("alt-f4", Quit, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-h", Hide, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-alt-h", HideOthers, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-m", Minimize, None),
    ]);

    cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
    cx.on_action(|_: &Hide, cx: &mut App| cx.hide());
    cx.on_action(|_: &HideOthers, cx: &mut App| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx: &mut App| cx.unhide_other_apps());
}

fn build_menus() -> Vec<Menu> {
    vec![
        Menu::new("Auru PM").items(application_menu_items()),
        Menu::new("File").items([
            disabled_item("Open Project…"),
            disabled_item("Clone Repository…"),
            MenuItem::separator(),
            disabled_item("Sync All"),
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseWindow),
        ]),
        Menu::new("Edit").items([
            disabled_item("Undo"),
            disabled_item("Redo"),
            MenuItem::separator(),
            disabled_item("Cut"),
            disabled_item("Copy"),
            disabled_item("Paste"),
            MenuItem::separator(),
            disabled_item("Select All"),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
        ]),
        Menu::new("Help").items([
            disabled_item("Auru PM Help"),
            MenuItem::separator(),
            disabled_item("Report an Issue…"),
        ]),
    ]
}

fn application_menu_items() -> Vec<MenuItem> {
    let mut items = vec![
        disabled_item("About Auru PM"),
        MenuItem::action("Settings…", OpenSettings),
        MenuItem::separator(),
    ];

    #[cfg(target_os = "macos")]
    items.extend([
        MenuItem::os_submenu("Services", SystemMenuType::Services),
        MenuItem::separator(),
        MenuItem::action("Hide Auru PM", Hide),
        MenuItem::action("Hide Others", HideOthers),
        MenuItem::action("Show All", ShowAll),
        MenuItem::separator(),
    ]);

    items.push(MenuItem::action("Quit Auru PM", Quit));
    items
}

fn disabled_item(label: &'static str) -> MenuItem {
    MenuItem::action(label, NoAction).disabled(true)
}

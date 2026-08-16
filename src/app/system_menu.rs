#[cfg(target_os = "macos")]
use gpui::KeyBinding;
use gpui::{App, Menu, MenuItem};
use rust_i18n::t;

gpui::actions!(
    ashell_system_menu,
    [
        AboutAshell,
        CloseWindow,
        HideAshell,
        HideOtherApplications,
        MinimizeWindow,
        NewLocalTerminal,
        OpenProjectWebsite,
        ShowAllApplications,
        ToggleFullScreen,
        ZoomWindow
    ]
);

const PROJECT_URL: &str = "https://github.com/rust-kotlin/ashell";

pub(crate) fn init(cx: &mut App) {
    #[cfg(target_os = "macos")]
    {
        cx.on_action(|_: &HideAshell, cx| cx.hide());
        cx.on_action(|_: &HideOtherApplications, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAllApplications, cx| cx.unhide_other_apps());
        cx.bind_keys([
            KeyBinding::new("cmd-h", HideAshell, None),
            KeyBinding::new("alt-cmd-h", HideOtherApplications, None),
            KeyBinding::new("cmd-m", MinimizeWindow, None),
            KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
        ]);
    }

    cx.on_action(|_: &OpenProjectWebsite, _| {
        if let Err(error) = open::that(PROJECT_URL) {
            tracing::warn!("failed to open Ashell project website: {error}");
        }
    });
}

pub(crate) fn set_app_menus(cx: &mut App) {
    let mut application_items = vec![
        MenuItem::action(t!("menu_about_ashell").to_string(), AboutAshell),
        MenuItem::separator(),
        MenuItem::action(t!("settings").to_string(), crate::OpenSettings),
    ];

    #[cfg(target_os = "macos")]
    application_items.extend([
        MenuItem::separator(),
        MenuItem::os_submenu(
            t!("menu_services").to_string(),
            gpui::SystemMenuType::Services,
        ),
        MenuItem::separator(),
        MenuItem::action(t!("menu_hide_ashell").to_string(), HideAshell),
        MenuItem::action(t!("menu_hide_others").to_string(), HideOtherApplications),
        MenuItem::action(t!("menu_show_all").to_string(), ShowAllApplications),
    ]);

    application_items.extend([
        MenuItem::separator(),
        MenuItem::action(t!("menu_quit_ashell").to_string(), crate::QuitApplication),
    ]);

    cx.set_menus([
        Menu::new("Ashell").items(application_items),
        Menu::new(t!("menu_file").to_string()).items([
            MenuItem::action(t!("local_terminal").to_string(), NewLocalTerminal),
            MenuItem::action(t!("new_connection").to_string(), crate::NewSsh),
            MenuItem::separator(),
            MenuItem::action(t!("menu_close_window").to_string(), CloseWindow),
        ]),
        Menu::new(t!("menu_edit").to_string()).items([
            MenuItem::os_action(
                t!("menu_undo").to_string(),
                gpui_component::input::Undo,
                gpui::OsAction::Undo,
            ),
            MenuItem::os_action(
                t!("menu_redo").to_string(),
                gpui_component::input::Redo,
                gpui::OsAction::Redo,
            ),
            MenuItem::separator(),
            MenuItem::os_action(
                t!("menu_cut").to_string(),
                gpui_component::input::Cut,
                gpui::OsAction::Cut,
            ),
            MenuItem::os_action(
                t!("menu_copy").to_string(),
                crate::Copy,
                gpui::OsAction::Copy,
            ),
            MenuItem::os_action(
                t!("menu_paste").to_string(),
                crate::Paste,
                gpui::OsAction::Paste,
            ),
            MenuItem::separator(),
            MenuItem::os_action(
                t!("menu_select_all").to_string(),
                gpui_component::input::SelectAll,
                gpui::OsAction::SelectAll,
            ),
        ]),
        Menu::new(t!("menu_view").to_string()).items([
            MenuItem::action(t!("search").to_string(), crate::OpenSearch),
            MenuItem::action(t!("menu_toggle_sidebar").to_string(), crate::ToggleSidebar),
            MenuItem::action(
                t!("settings_open_transfers").to_string(),
                crate::OpenTransfers,
            ),
            MenuItem::separator(),
            MenuItem::action(t!("menu_toggle_full_screen").to_string(), ToggleFullScreen),
        ]),
        // GPUI recognizes this exact title as the native macOS Window menu.
        Menu::new("Window").items([
            MenuItem::action(t!("menu_minimize").to_string(), MinimizeWindow),
            MenuItem::action(t!("menu_zoom").to_string(), ZoomWindow),
        ]),
        Menu::new(t!("menu_help").to_string()).items([MenuItem::action(
            t!("menu_project_website").to_string(),
            OpenProjectWebsite,
        )]),
    ]);
}

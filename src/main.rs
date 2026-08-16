#![windows_subsystem = "windows"]

use gpui::KeyBinding;
use gpui_component_assets::Assets;

mod app;
mod backend;
mod desktop_notification;
mod session;
mod sftp;
mod sync;
mod system;
mod terminal;
mod text_encoding;

rust_i18n::i18n!("locales", fallback = "en");

gpui::actions!(ashell_terminal, [TerminalTabKey, TerminalBacktabKey]);

pub(crate) use app::keybinding_recorder::{
    ClosePane, Copy, FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp, NewSsh, OpenSearch,
    OpenSession, OpenSettings, OpenTransfers, Paste, QuitApplication, SplitPaneDown, SplitPaneLeft,
    SplitPaneRight, SplitPaneUp, ToggleSftpZoom, ToggleSidebar,
};
pub(crate) use app::system_menu::{
    AboutAshell, CloseWindow, MinimizeWindow, NewLocalTerminal, ToggleFullScreen, ZoomWindow,
};

pub(crate) use app::{Ashell, PaneLayout, SelectorEntry, SftpContextMenuState, TabGroup};

fn main() {
    app::startup::sync_macos_launch_environment();
    app::startup::init_logging();
    desktop_notification::initialize();

    #[cfg(target_os = "macos")]
    let app = gpui_platform::application()
        .with_assets(Assets)
        .with_quit_mode(gpui::QuitMode::LastWindowClosed);

    #[cfg(not(target_os = "macos"))]
    let app = gpui_platform::application().with_assets(Assets);

    app.on_reopen(|cx| {
        if cx.windows().is_empty() {
            app::startup::open_main_window(cx);
        }
    });
    app.run(move |cx| {
        gpui_component::init(cx);
        app::system_menu::init(cx);
        cx.on_action(|_: &QuitApplication, cx| cx.quit());

        cx.bind_keys([
            KeyBinding::new(
                "tab",
                TerminalTabKey,
                Some(app::constants::TERMINAL_KEY_CONTEXT),
            ),
            KeyBinding::new(
                "shift-tab",
                TerminalBacktabKey,
                Some(app::constants::TERMINAL_KEY_CONTEXT),
            ),
        ]);
        app::startup::bind_workspace_keys(cx);
        app::theme::load_embedded_themes(cx);
        if let Err(err) = app::theme::load_fonts(cx) {
            tracing::warn!("failed to load embedded fonts: {err:#}");
        }
        app::startup::open_main_window(cx);
    });
}

#![windows_subsystem = "windows"]

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ops::Range,
    process::Command,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};

use alacritty_terminal::index::Side;
use alacritty_terminal::selection::SelectionType;
use anyhow::{Context as _, Result};
use gpui::{
    Anchor, App, AppContext as _, Bounds, ClipboardItem, Context, ElementId, Entity, FocusHandle,
    Focusable as _, FontWeight, Hsla, InteractiveElement as _, IntoElement, KeyBinding,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _,
    PathPromptOptions, Pixels, Point, QuitMode, Render, ScrollDelta, ScrollWheelEvent,
    SharedString, Size, StatefulInteractiveElement, Styled as _, UniformListScrollHandle, Window, WindowOptions, div, point,
    prelude::FluentBuilder as _, px, rems, size, uniform_list,
};
use gpui_component::{
    ActiveTheme as _, Disableable, ElementExt, IconName, Root, Sizable as _, Theme, ThemeMode,
    ThemeRegistry, WindowExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    switch::Switch,
    dialog::Dialog,
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem},
    progress::Progress,
    resizable::{ResizableState, h_resizable, resizable_panel, v_resizable},
    scroll::{ScrollableElement as _, Scrollbar, ScrollbarHandle, ScrollbarShow},
    tab::{Tab, TabBar},
    v_flex,
};
use gpui_component_assets::Assets;
use rust_i18n::t;
use tokio::runtime::Runtime;
use uuid::Uuid;

mod config;
mod local_terminal;
mod sftp;
mod ssh_terminal;
mod system;
mod terminal;
mod terminal_element;

use config::{AuthMethod, ConfigStore, Session};
use sftp::{RemoteEntry, SftpHandle, format_mtime};
use system::{DiskSample, SystemSampler, SystemSnapshot, format_bytes};
use terminal::{BackendCommand, BackendEvent, TabKind, TerminalTab, encode_key};
use terminal_element::TerminalElement;

rust_i18n::i18n!("locales", fallback = "en");

const DEFAULT_COLS: u16 = 100;
const DEFAULT_ROWS: u16 = 30;

const SIDEBAR_WIDTH: f32 = 306.0;
const TAB_BAR_HEIGHT: f32 = 52.0;
const TERMINAL_PADDING_X: f32 = 32.0;
const TERMINAL_PADDING_Y: f32 = 32.0;
const TERMINAL_KEY_CONTEXT: &str = "AshellTerminal";
const EMBEDDED_THEME_JSONS: &[&str] = &[
    include_str!("../assets/themes/matrix.json"),
    include_str!("../assets/themes/tokyonight.json"),
    include_str!("../assets/themes/gruvbox.json"),
    include_str!("../assets/themes/solarized.json"),
];

gpui::actions!(ashell_terminal, [TerminalTabKey, TerminalBacktabKey]);

#[derive(Debug, Clone, Copy)]
struct TerminalScrollbarState {
    line_height: Pixels,
    total_lines: usize,
    viewport_lines: usize,
    display_offset: usize,
}

#[derive(Clone, Default)]
struct TerminalScrollbarHandle {
    state: Rc<RefCell<Option<TerminalScrollbarState>>>,
    future_display_offset: Rc<Cell<Option<usize>>>,
}

impl TerminalScrollbarHandle {
    fn update(&self, snapshot: &terminal::RenderSnapshot, line_height: Pixels) {
        self.state.replace(Some(TerminalScrollbarState {
            line_height,
            total_lines: snapshot.history_size + snapshot.rows,
            viewport_lines: snapshot.rows,
            display_offset: snapshot.display_offset,
        }));
    }
}

impl ScrollbarHandle for TerminalScrollbarHandle {
    fn offset(&self) -> Point<Pixels> {
        let Some(state) = *self.state.borrow() else {
            return point(px(0.), px(0.));
        };
        let scroll_offset = state
            .total_lines
            .saturating_sub(state.viewport_lines)
            .saturating_sub(state.display_offset);
        point(px(0.), -(scroll_offset as f32 * state.line_height))
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        let Some(state) = *self.state.borrow() else {
            return;
        };
        let offset_delta = (offset.y / state.line_height).round() as i32;
        let max_offset = state.total_lines.saturating_sub(state.viewport_lines);
        let display_offset = (max_offset as i32 + offset_delta).clamp(0, max_offset as i32);
        self.future_display_offset
            .set(Some(display_offset as usize));
    }

    fn content_size(&self) -> Size<Pixels> {
        let Some(state) = *self.state.borrow() else {
            return size(px(0.), px(0.));
        };
        size(
            px(0.),
            state.total_lines.max(state.viewport_lines) as f32 * state.line_height,
        )
    }
}



struct Ashell {
    focus_handle: FocusHandle,
    selector_focus_handle: FocusHandle,
    host_input: Entity<InputState>,
    session_name_input: Entity<InputState>,
    port_input: Entity<InputState>,
    user_input: Entity<InputState>,
    password_input: Entity<InputState>,
    key_path_input: Entity<InputState>,
    key_inline_input: Entity<InputState>,
    sftp_path_input: Entity<InputState>,
    ssh_auth_method: AuthMethod,
    editing_session_id: Option<String>,
    follow_system_theme: bool,
    theme_mode: ThemeMode,
    light_theme_name: SharedString,
    dark_theme_name: SharedString,
    ui_font_size: f32,
    terminal_font_size: f32,
    ui_font_family: SharedString,
    terminal_font_family: SharedString,
    tabs: Vec<TerminalTab>,
    sftp_handles: HashMap<String, SftpHandle>,
    active_tab: Option<String>,
    selector_selection: usize,
    workspace_panels: Entity<ResizableState>,
    body_panels: Entity<ResizableState>,
    terminal_scrollbar: TerminalScrollbarHandle,
    remote_files_scroll_handle: UniformListScrollHandle,
    tabs_scroll_handle: gpui::ScrollHandle,
    selector_scroll_handle: gpui::ScrollHandle,
    saved_scroll_handle: gpui::ScrollHandle,
    connection_progress: Option<ConnectionProgress>,
    pending_sftp_path_sync: Option<String>,
    sftp_context_menu: Option<SftpContextMenuState>,
    sftp_creating_folder: bool,
    sftp_new_folder_input: Entity<InputState>,
    sftp_delete_scroll_handle: gpui::ScrollHandle,
    show_hidden_files: bool,
    transfers: Vec<crate::terminal::Transfer>,
    show_transfers_dialog: bool,
    system_status: Option<SharedString>,
    terminal_bounds: Option<Bounds<Pixels>>,
    terminal_selecting: bool,
    terminal_marked_text: Option<String>,
    status: SharedString,
    config: ConfigStore,
    system_sampler: SystemSampler,
    system: SystemSnapshot,
    last_system_sample: Instant,
    last_theme_sync: Instant,
    last_window_appearance: Option<gpui::WindowAppearance>,
    remote_sample_in_flight: bool,
    runtime: Runtime,
    events_rx: mpsc::Receiver<BackendEvent>,
    events_tx: mpsc::Sender<BackendEvent>,
    _subscriptions: Vec<gpui::Subscription>,
}

#[derive(Clone)]
enum SelectorEntry {
    Local,
    NewSsh,
    Saved(String),
}

#[derive(Clone)]
struct ConnectionProgress {
    tab_id: String,
    title: SharedString,
    lines: Vec<SharedString>,
    failed: bool,
}

#[derive(Clone)]
struct SftpContextMenuState {
    remote_path: String,
    is_dir: bool,
    position: Point<Pixels>,
}

impl Ashell {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let host_input = cx.new(|cx| InputState::new(window, cx).placeholder(t!("host")));
        let session_name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("name (optional)"));
        let port_input = cx.new(|cx| InputState::new(window, cx).default_value("22"));
        let user_input = cx.new(|cx| InputState::new(window, cx).default_value("root"));
        let password_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("password"))
                .masked(true)
        });
        let key_path_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("~/.ssh/id_ed25519"));
        let key_inline_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(5)
                .placeholder("-----BEGIN OPENSSH PRIVATE KEY-----")
        });
        let sftp_path_input = cx.new(|cx| InputState::new(window, cx).default_value("/"));
        let sftp_new_folder_input = cx.new(|cx| InputState::new(window, cx).placeholder(t!("new_folder").to_string()));

        let _subscriptions = vec![
            cx.subscribe_in(&host_input, window, Self::on_input_event),
            cx.subscribe_in(&session_name_input, window, Self::on_input_event),
            cx.subscribe_in(&port_input, window, Self::on_input_event),
            cx.subscribe_in(&user_input, window, Self::on_input_event),
            cx.subscribe_in(&password_input, window, Self::on_input_event),
            cx.subscribe_in(&key_path_input, window, Self::on_input_event),
            cx.subscribe_in(&key_inline_input, window, Self::on_input_event),
            cx.subscribe_in(&sftp_path_input, window, Self::on_input_event),
            cx.subscribe_in(&sftp_new_folder_input, window, Self::on_input_event),
        ];

        let (events_tx, events_rx) = mpsc::channel();
        let workspace_panels = cx.new(|_| ResizableState::default());
        let body_panels = cx.new(|_| ResizableState::default());
        let mut system_sampler = SystemSampler::new();
        let system = system_sampler.sample();
        let default_light_theme_name = ThemeRegistry::global(cx).default_light_theme().name.clone();
        let default_dark_theme_name = ThemeRegistry::global(cx).default_dark_theme().name.clone();
        let config = ConfigStore::load().unwrap_or_else(|err| {
            tracing::warn!("failed to load config: {err:#}");
            ConfigStore::in_memory()
        });
        let follow_system_theme =
            if config.light_theme_name().is_empty() && config.dark_theme_name().is_empty() {
                true
            } else {
                config.follow_system_theme()
            };

        let theme_mode = match config.theme_mode() {
            "light" => ThemeMode::Light,
            "dark" => ThemeMode::Dark,
            _ => ThemeMode::Light,
        };
        let light_theme_name = if config.light_theme_name().is_empty() {
            default_light_theme_name
        } else {
            config.light_theme_name().into()
        };
        let dark_theme_name = if config.dark_theme_name().is_empty() {
            default_dark_theme_name
        } else {
            config.dark_theme_name().into()
        };

        let configured_locale = config.locale();
        let mut active_locale = configured_locale.to_string();
        if active_locale == "system" {
            active_locale = sys_locale::get_locale().unwrap_or_else(|| "en".to_string());
            if active_locale.starts_with("zh") {
                active_locale = "zh-CN".to_string();
            } else {
                active_locale = "en".to_string();
            }
        }
        rust_i18n::set_locale(&active_locale);
        gpui_component::set_locale(&active_locale);
        let ui_font_family: SharedString = config.ui_font_family().into();
        let terminal_font_family: SharedString = config.terminal_font_family().into();
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            selector_focus_handle: cx.focus_handle(),
            host_input,
            session_name_input,
            port_input,
            user_input,
            password_input,
            key_path_input,
            key_inline_input,
            sftp_path_input,
            ssh_auth_method: AuthMethod::Password,
            editing_session_id: None,
            follow_system_theme,
            theme_mode,
            light_theme_name,
            dark_theme_name,
            ui_font_size: config.ui_font_size(),
            terminal_font_size: config.terminal_font_size(),
            ui_font_family,
            terminal_font_family,
            tabs: Vec::new(),
            sftp_handles: HashMap::new(),
            active_tab: None,
            selector_selection: 0,
            workspace_panels,
            body_panels,
            terminal_scrollbar: TerminalScrollbarHandle::default(),
            remote_files_scroll_handle: UniformListScrollHandle::new(),
            tabs_scroll_handle: gpui::ScrollHandle::new(),
            selector_scroll_handle: gpui::ScrollHandle::new(),
            saved_scroll_handle: gpui::ScrollHandle::new(),
            connection_progress: None,
            pending_sftp_path_sync: Some("/".into()),
            sftp_context_menu: None,
            sftp_creating_folder: false,
            sftp_new_folder_input,
            sftp_delete_scroll_handle: gpui::ScrollHandle::new(),
            show_hidden_files: false,
            transfers: config.transfers(),
            show_transfers_dialog: false,
            system_status: None,
            terminal_bounds: None,
            terminal_selecting: false,
            terminal_marked_text: None,
            status: "ready".into(),
            config,
            system_sampler,
            system,
            last_system_sample: Instant::now(),
            last_theme_sync: Instant::now(),
            last_window_appearance: None,
            remote_sample_in_flight: false,
            runtime: Runtime::new().expect("create tokio runtime"),
            events_rx,
            events_tx,
            _subscriptions,
        };

        this.apply_theme_preferences(window, cx);
        // this.open_local(cx);
        this.start_event_pump(cx);
        this
    }

    fn on_input_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if input == &self.sftp_path_input {
            if let InputEvent::PressEnter { .. } = event {
                let path = self
                    .sftp_path_input
                    .read(cx)
                    .text()
                    .to_string()
                    .trim()
                    .to_string();
                self.navigate_sftp(if path.is_empty() { "/".into() } else { path }, cx);
                window.prevent_default();
                cx.stop_propagation();
            }
        } else if input == &self.sftp_new_folder_input {
            match event {
                InputEvent::PressEnter { .. } => {
                    let name = self.sftp_new_folder_input.read(cx).text().to_string();
                    if !name.is_empty() {
                        let base_path = self.sftp_path_input.read(cx).text().to_string();
                        let path = crate::sftp::join_remote(&base_path, &name);
                        if let Some(id) = self.active_tab.clone() {
                            if let Some(handle) = self.sftp_handles.get(&id) {
                                let _ = handle.commands.send(crate::sftp::SftpCommand::CreateDir(path));
                            }
                        }
                    }
                    self.sftp_creating_folder = false;
                    window.prevent_default();
                    cx.stop_propagation();
                }
                InputEvent::Blur => {
                    self.sftp_creating_folder = false;
                }
                _ => {}
            }
        }
        cx.notify();
    }

    fn start_event_pump(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                if this
                    .update(cx, |this, cx| {
                        this.drain_backend_events();
                        this.sample_system_if_due();
                        this.sync_theme_if_due(cx);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn drain_backend_events(&mut self) {
        let mut transfers_changed = false;
        while let Ok(event) = self.events_rx.try_recv() {
            match event {
                BackendEvent::Output { tab_id, bytes } => {
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                        tab.feed(&bytes);
                    }
                }
                BackendEvent::Status { tab_id, text } => {
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                        tab.status = text.clone();
                    }
                    if let Some(progress) = self.connection_progress.as_mut() {
                        if progress.tab_id == tab_id {
                            progress.lines.push(text.clone().into());
                        }
                    }
                    self.status = text.into();
                }
                BackendEvent::Connected { tab_id } => {
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                        tab.connected = true;
                    }
                    self.request_active_system_snapshot();
                    if self
                        .connection_progress
                        .as_ref()
                        .is_some_and(|progress| progress.tab_id == tab_id)
                    {
                        self.connection_progress = None;
                    }
                }
                BackendEvent::SftpEntries {
                    tab_id,
                    path,
                    entries,
                } => {
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                        if let Some(sftp) = tab.sftp.as_mut() {
                            sftp.current_path = path;
                            sftp.entries = entries;
                            if self.active_tab.as_deref() == Some(tab_id.as_str()) {
                                self.pending_sftp_path_sync = Some(sftp.current_path.clone());
                            }
                        }
                    }
                }
                BackendEvent::SftpPreview { tab_id, preview } => {
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                        if let Some(sftp) = tab.sftp.as_mut() {
                            sftp.selected_path = Some(preview.path.clone());
                            sftp.preview = Some(preview);
                        }
                    }
                }
                BackendEvent::SftpStatus { tab_id, text } => {
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                        if let Some(sftp) = tab.sftp.as_mut() {
                            sftp.status = text.clone();
                        }
                    }
                    self.status = text.into();
                }
                BackendEvent::RemoteSystem { tab_id, snapshot } => {
                    self.remote_sample_in_flight = false;
                    if self.active_tab.as_deref() == Some(tab_id.as_str()) {
                        self.system_status = None;
                        self.system = snapshot;
                    }
                }
                BackendEvent::RemoteSystemUnavailable { tab_id, reason } => {
                    self.remote_sample_in_flight = false;
                    if self.active_tab.as_deref() == Some(tab_id.as_str()) {
                        self.system_status = Some(reason.clone().into());
                        self.status = reason.into();
                    }
                }
                BackendEvent::Closed { tab_id, reason } => {
                    self.remote_sample_in_flight = false;
                    let mut tab_title = None;
                    let mut session_label = None;
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                        tab.connected = false;
                        tab.status = reason.clone();
                        tab_title = Some(tab.title.clone());
                        session_label = tab.session.as_ref().map(|session| {
                            format!("{}@{}:{}", session.user, session.host, session.port)
                        });
                    }
                    if self.active_tab.as_deref() == Some(tab_id.as_str()) {
                        self.system_status = Some(reason.clone().into());
                    }
                    let is_graceful_exit = reason == "Terminal process exited" || reason == "SSH session exited";
                    if let Some(progress) = self.connection_progress.as_mut() {
                        if progress.tab_id == tab_id {
                            progress.lines.push(reason.clone().into());
                            let _ = session_label;
                            let _ = tab_title;
                            progress.title = t!("connection_failed").into();
                            progress.failed = true;
                        }
                    } else if let Some(_) = session_label {
                        if !is_graceful_exit {
                            self.connection_progress = Some(ConnectionProgress {
                                tab_id: tab_id.clone(),
                                title: t!("connection_failed").into(),
                                lines: vec![reason.clone().into()],
                                failed: true,
                            });
                        }
                    }
                    self.status = reason.into();
                }
                BackendEvent::SftpHome { tab_id, home } => {
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                        if let Some(sftp) = tab.sftp.as_mut() {
                            sftp.home_dir = home;
                        }
                    }
                }
                BackendEvent::TerminalTitleChanged { tab_id, title } => {
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                        if let Some(sftp) = &tab.sftp {
                            let potential_path = if let Some((_, p)) = title.rsplit_once(':') {
                                p.trim()
                            } else {
                                title.trim()
                            };

                            if potential_path.starts_with('/') || potential_path.starts_with('~') {
                                let path_to_sync = if potential_path == "~" {
                                    sftp.home_dir.clone()
                                } else if let Some(rest) = potential_path.strip_prefix("~/") {
                                    crate::sftp::join_remote(&sftp.home_dir, rest)
                                } else {
                                    potential_path.to_string()
                                };
                                
                                if path_to_sync != sftp.current_path {
                                    if let Some(handle) = self.sftp_handles.get(&tab_id) {
                                        handle.list_dir(path_to_sync);
                                    }
                                }
                            }
                        }
                    }
                }
                BackendEvent::TransferStarted { tab_id, info } => {
                    let tab_title = self.tabs.iter().find(|t| t.id == tab_id).map(|t| t.title.clone()).unwrap_or_else(|| "Unknown".to_string());
                    self.transfers.insert(
                        0,
                        crate::terminal::Transfer {
                            tab_id,
                            tab_title,
                            info,
                            transferred: 0,
                            total: None,
                            state: crate::terminal::TransferState::Running,
                        },
                    );
                    if self.transfers.len() > 50 {
                        self.transfers.truncate(50);
                    }
                    self.show_transfers_dialog = true;
                    transfers_changed = true;
                }
                BackendEvent::TransferProgress {
                    tab_id: _,
                    id,
                    transferred,
                    total,
                    state,
                } => {
                    if let Some(t) = self.transfers.iter_mut().find(|t| t.info.id == id) {
                        t.transferred = transferred;
                        if let Some(total) = total {
                            t.total = Some(total);
                        }
                        t.state = state;
                        transfers_changed = true;
                    }
                }
            }
        }
        if transfers_changed {
            self.config.set_transfers(self.transfers.clone());
        }
    }

    fn sample_system_if_due(&mut self) {
        if self.last_system_sample.elapsed() >= SystemSampler::interval() {
            self.request_active_system_snapshot();
        }
    }

    fn sync_theme_if_due(&mut self, cx: &mut Context<Self>) {
        if self.follow_system_theme && self.last_theme_sync.elapsed() >= Duration::from_secs(1) {
            let appearance = cx.window_appearance();
            if self.last_window_appearance != Some(appearance) {
                Theme::sync_system_appearance(None, cx);
                self.last_window_appearance = Some(appearance);
            }
            self.last_theme_sync = Instant::now();
        }
    }

    fn request_active_system_snapshot(&mut self) {
        if let Some((tab_id, session)) = self.active_ssh_session() {
            if self.remote_sample_in_flight {
                return;
            }
            self.remote_sample_in_flight = true;
            let events = self.events_tx.clone();
            self.runtime.spawn(async move {
                match ssh_terminal::sample_remote_system(session).await {
                    Ok(snapshot) => {
                        let _ = events.send(BackendEvent::RemoteSystem { tab_id, snapshot });
                    }
                    Err(err) => {
                        let _ = events.send(BackendEvent::RemoteSystemUnavailable {
                            tab_id,
                            reason: format!("remote metrics unavailable: {err:#}"),
                        });
                    }
                }
            });
        } else {
            self.system = self.system_sampler.sample();
            self.system_status = None;
            self.remote_sample_in_flight = false;
        }
        self.last_system_sample = Instant::now();
    }

    fn open_local(&mut self, cx: &mut Context<Self>) {
        let id = Uuid::new_v4().to_string();
        match local_terminal::spawn_local_terminal(
            id.clone(),
            DEFAULT_COLS,
            DEFAULT_ROWS,
            self.events_tx.clone(),
        ) {
            Ok(backend) => {
                let title = if cfg!(windows) { "PowerShell" } else { "Local" }.to_string();
                let mut tab =
                    TerminalTab::new_local(id.clone(), title, backend, self.events_tx.clone());
                tab.resize(DEFAULT_COLS, DEFAULT_ROWS);
                self.tabs.push(tab);
                self.active_tab = Some(id);
                self.tabs_scroll_handle.scroll_to_item(self.tabs.len() - 1);
                self.status = "local terminal opened".into();
            }
            Err(err) => {
                self.status = format!("failed to open local terminal: {err:#}").into();
            }
        }
        cx.notify();
    }

    fn connect_ssh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session_name = self.session_name_input.read(cx).value().trim().to_string();
        let host = self.host_input.read(cx).value().trim().to_string();
        let port = self
            .port_input
            .read(cx)
            .value()
            .trim()
            .parse::<u16>()
            .unwrap_or(22);
        let user = self.user_input.read(cx).value().trim().to_string();
        let password = self.password_input.read(cx).value().to_string();
        let key_path = self.key_path_input.read(cx).value().trim().to_string();
        let key_inline = self.key_inline_input.read(cx).value().to_string();

        if host.is_empty() || user.is_empty() {
            self.status = t!("host_and_user_required").into();
            cx.notify();
            return;
        }

        let name = if session_name.is_empty() {
            host.clone()
        } else {
            session_name
        };
        let existing_id = self.editing_session_id.clone();
        let existing_last_used = existing_id
            .as_deref()
            .and_then(|id| self.config.get(id))
            .and_then(|session| session.last_used.clone());

        let mut session = match self.ssh_auth_method {
            AuthMethod::Password => Session::password(host, port, user, password),
            AuthMethod::Key => {
                if key_path.is_empty() && key_inline.trim().is_empty() {
                    self.status = "private key path or content is required".into();
                    cx.notify();
                    return;
                }
                Session::key(host, port, user, key_path, key_inline)
            }
        };
        session.name = name;
        if let Some(id) = existing_id {
            session.id = id;
        }
        session.last_used = existing_last_used;
        self.config.upsert(session.clone());
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save config: {err:#}");
        }

        self.open_ssh_session(session, cx);
        self.editing_session_id = None;
        window.close_dialog(cx);
    }

    fn set_input_value(
        input: &Entity<InputState>,
        value: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        input.update(cx, |state, cx| state.set_value(value, window, cx));
    }

    fn reset_ssh_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing_session_id = None;
        self.ssh_auth_method = AuthMethod::Password;
        Self::set_input_value(&self.session_name_input, "", window, cx);
        Self::set_input_value(&self.host_input, "", window, cx);
        Self::set_input_value(&self.port_input, "22", window, cx);
        Self::set_input_value(&self.user_input, "root", window, cx);
        Self::set_input_value(&self.password_input, "", window, cx);
        Self::set_input_value(&self.key_path_input, "", window, cx);
        Self::set_input_value(&self.key_inline_input, "", window, cx);
    }

    fn load_session_into_form(
        &mut self,
        session: &Session,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editing_session_id = Some(session.id.clone());
        self.ssh_auth_method = session.auth;
        Self::set_input_value(&self.session_name_input, session.name.clone(), window, cx);
        Self::set_input_value(&self.host_input, session.host.clone(), window, cx);
        Self::set_input_value(&self.port_input, session.port.to_string(), window, cx);
        Self::set_input_value(&self.user_input, session.user.clone(), window, cx);
        Self::set_input_value(&self.password_input, session.password.clone(), window, cx);
        Self::set_input_value(
            &self.key_path_input,
            session.private_key_path.clone(),
            window,
            cx,
        );
        Self::set_input_value(
            &self.key_inline_input,
            session.private_key_inline.clone(),
            window,
            cx,
        );
    }

    fn open_new_ssh_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reset_ssh_form(window, cx);
        self.show_ssh_dialog(window, cx);
    }

    fn edit_saved_session(
        &mut self,
        session_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.config.get(&session_id).cloned() else {
            self.status = "saved session not found".into();
            cx.notify();
            return;
        };
        self.load_session_into_form(&session, window, cx);
        self.show_ssh_dialog(window, cx);
    }

    fn terminal_cell_width(&self) -> f32 {
        (self.terminal_font_size * 0.646).max(6.0)
    }

    fn terminal_line_height(&self) -> f32 {
        (self.terminal_font_size * 1.385).max(self.terminal_font_size + 2.0)
    }

    fn change_terminal_font_size(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.terminal_font_size = (self.terminal_font_size + delta).clamp(10.0, 24.0);
        self.config.set_terminal_font_size(self.terminal_font_size);
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save terminal font size: {err:#}");
        }
        self.status = format!("terminal font size: {:.0}px", self.terminal_font_size).into();
        cx.notify();
    }

    fn change_ui_font_size(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.ui_font_size = (self.ui_font_size + delta).clamp(8.0, 24.0);
        self.config.set_ui_font_size(self.ui_font_size);
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save UI font size: {err:#}");
        }
        Theme::global_mut(cx).font_size = px(self.ui_font_size);
        self.status = format!("UI font size: {:.0}px", self.ui_font_size).into();
        cx.notify();
    }

    fn change_ui_font_family(
        &mut self,
        family: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ui_font_family = family.into();
        self.config.set_ui_font_family(family);
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save UI font family: {err:#}");
        }
        set_theme_font_names(Theme::global_mut(cx), &self.ui_font_family);
        cx.notify();
        window.refresh();
    }

    fn change_terminal_font_family(&mut self, family: &str, cx: &mut Context<Self>) {
        self.terminal_font_family = family.into();
        self.config.set_terminal_font_family(family);
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save terminal font family: {err:#}");
        }
        cx.notify();
    }

    fn show_ssh_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity();
        let session_name_input = self.session_name_input.clone();
        let host_input = self.host_input.clone();
        let focus_host_input = host_input.clone();
        let port_input = self.port_input.clone();
        let user_input = self.user_input.clone();
        let password_input = self.password_input.clone();
        let key_path_input = self.key_path_input.clone();
        let key_inline_input = self.key_inline_input.clone();

        window.open_dialog(cx, move |dialog: Dialog, _window, _cx| {
            dialog
                .title(t!("new_ssh_connection"))
                .w(px(520.))
                .overlay_closable(true)
                .content({
                    let view = view.clone();
                    let session_name_input = session_name_input.clone();
                    let host_input = host_input.clone();
                    let port_input = port_input.clone();
                    let user_input = user_input.clone();
                    let password_input = password_input.clone();
                    let key_path_input = key_path_input.clone();
                    let key_inline_input = key_inline_input.clone();
                    move |content, window, cx| {
                        let is_password = view.read(cx).ssh_auth_method == AuthMethod::Password;
                        let is_editing = view.read(cx).editing_session_id.is_some();
                        content.child(
                            v_flex()
                                .gap_3()
                                .child(Input::new(&session_name_input).tab_index(0))
                                .child(Input::new(&host_input).tab_index(1))
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .child(Input::new(&port_input).w(px(96.)).tab_index(2))
                                        .child(Input::new(&user_input).flex_1().tab_index(3)),
                                )
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .child(
                                            Button::new("ssh-auth-password")
                                                .label(t!("password").to_string())
                                                .when(is_password, |button| button.primary())
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, _, cx| {
                                                        this.set_ssh_auth_method(
                                                            AuthMethod::Password,
                                                            cx,
                                                        )
                                                    },
                                                )),
                                        )
                                        .child(
                                            Button::new("ssh-auth-key")
                                                .label(t!("key").to_string())
                                                .when(!is_password, |button| button.primary())
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, _, cx| {
                                                        this.set_ssh_auth_method(
                                                            AuthMethod::Key,
                                                            cx,
                                                        )
                                                    },
                                                )),
                                        ),
                                )
                                .when(is_password, |this| {
                                    this.child(
                                        Input::new(&password_input).mask_toggle().tab_index(4),
                                    )
                                })
                                .when(!is_password, |this| {
                                    this.child(Input::new(&key_path_input).tab_index(5)).child(
                                        Input::new(&key_inline_input).h(px(128.)).tab_index(6),
                                    )
                                })
                                .child(
                                    h_flex()
                                        .justify_end()
                                        .gap_2()
                                        .child(
                                            Button::new("connect-ssh-cancel")
                                                .label(t!("cancel").to_string())
                                                .on_click(|_, window, cx| window.close_dialog(cx)),
                                        )
                                        .child(
                                            Button::new("connect-ssh-confirm")
                                                .primary()
                                                .label(if is_editing {
                                                    t!("save")
                                                } else {
                                                    t!("connect")
                                                })
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, window, cx| {
                                                        this.connect_ssh(window, cx)
                                                    },
                                                )),
                                        ),
                                ),
                        )
                    }
                })
        });
        window.defer(cx, move |window, cx| {
            window.focus(&focus_host_input.read(cx).focus_handle(cx), cx);
        });
    }

    fn show_selector_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity();
        let selector_focus_handle = self.selector_focus_handle.clone();
        let deferred_selector_focus_handle = selector_focus_handle.clone();
        let sessions = self.config.sessions().to_vec();
        let active_session_id = self.active_session_id().map(ToOwned::to_owned);
        self.selector_selection = self.default_selector_index();
        window.open_dialog(cx, move |dialog: Dialog, _window, _| {
            dialog
                .title(t!("open_session").to_string())
                .w(px(520.))
                .on_ok({
                    let view = view.clone();
                    move |_, window, cx| {
                        view.update(cx, |this, cx| {
                            this.activate_selector_selection(window, cx);
                        });
                        false
                    }
                })
                .content({
                    let view = view.clone();
                    let sessions = sessions.clone();
                    let _active_session_id = active_session_id.clone();
                    let selector_focus_handle = selector_focus_handle.clone();
                    move |content, window, _cx| {
                        let selected_index = view.read(_cx).selector_selection;
                        let scroll_handle = view.read(_cx).selector_scroll_handle.clone();
                        content.child(
                            v_flex()
                                .track_focus(&selector_focus_handle)
                                .on_key_down(window.listener_for(
                                    &view,
                                    |this, event, window, cx| {
                                        this.on_selector_key_down(event, window, cx)
                                    },
                                ))
                                .gap_2()
                                .child(
                                    div()
                                        .w_full()
                                        .p_2()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(if selected_index == 0 {
                                            _cx.theme().primary
                                        } else {
                                            _cx.theme().border
                                        })
                                        .bg(if selected_index == 0 {
                                            _cx.theme().tab_active
                                        } else {
                                            _cx.theme().muted
                                        })
                                        .cursor_pointer()
                                        .hover(|this| this.bg(_cx.theme().secondary))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            window.listener_for(&view, |this, _, window, cx| {
                                                this.open_local(cx);
                                                window.close_dialog(cx);
                                            }),
                                        )
                                        .child(
                                            v_flex()
                                                .gap_1()
                                                .child(
                                                    div()
                                                        .text_size(rems(1.0))
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .child(t!("local_terminal")),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(rems(0.917))
                                                        .text_color(_cx.theme().muted_foreground)
                                                        .child(t!("open_local_shell_tab")),
                                                ),
                                        ),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .p_2()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(if selected_index == 1 {
                                            _cx.theme().primary
                                        } else {
                                            _cx.theme().border
                                        })
                                        .bg(if selected_index == 1 {
                                            _cx.theme().tab_active
                                        } else {
                                            _cx.theme().muted
                                        })
                                        .cursor_pointer()
                                        .hover(|this| this.bg(_cx.theme().secondary))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            window.listener_for(&view, |this, _, window, cx| {
                                                window.close_dialog(cx);
                                                this.open_new_ssh_dialog(window, cx);
                                            }),
                                        )
                                        .child(
                                            v_flex()
                                                .gap_1()
                                                .child(
                                                    div()
                                                        .text_size(rems(1.0))
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .child(t!("new_ssh_connection")),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(rems(0.917))
                                                        .text_color(_cx.theme().muted_foreground)
                                                        .child(t!("create_or_edit_ssh_session")),
                                                ),
                                        ),
                                )
                                .child(
                                    div()
                                        .relative()
                                        .max_h(px(320.))
                                        .size_full()
                                        .child(
                                            v_flex()
                                                .size_full()
                                                .id("selector-scroll-view")
                                                .track_scroll(&scroll_handle)
                                                .overflow_y_scroll()
                                                .gap_2()
                                                .children(sessions.clone().into_iter().enumerate().map(
                                                    |(ix, session)| {
                                                        let connect_id = session.id.clone();
                                                        let is_selected = selected_index == ix + 2;
                                                        let name = session.name.clone();
                                                        let detail = format!(
                                                            "{}@{}:{}",
                                                            session.user, session.host, session.port
                                                        );
                                                div()
                                                    .id(("selector-open", ix))
                                                    .w_full()
                                                    .p_2()
                                                    .rounded_md()
                                                    .border_1()
                                                    .border_color(if is_selected {
                                                        _cx.theme().primary
                                                    } else {
                                                        _cx.theme().border
                                                    })
                                                    .bg(if is_selected {
                                                        _cx.theme().tab_active
                                                    } else {
                                                        _cx.theme().muted
                                                    })
                                                    .cursor_pointer()
                                                    .hover(|this| this.bg(_cx.theme().secondary))
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        window.listener_for(
                                                            &view,
                                                            move |this, _, window, cx| {
                                                                this.connect_saved_session(
                                                                    connect_id.clone(),
                                                                    cx,
                                                                );
                                                                window.close_dialog(cx);
                                                            },
                                                        ),
                                                    )
                                                    .child(
                                                        v_flex()
                                                            .gap_1()
                                                            .child(
                                                                div()
                                                                    .text_size(rems(1.0))
                                                                    .font_weight(
                                                                        FontWeight::SEMIBOLD,
                                                                    )
                                                                    .child(name),
                                                            )
                                                            .child(
                                                                div()
                                                                    .text_size(rems(0.917))
                                                                    .text_color(
                                                                        _cx.theme()
                                                                            .muted_foreground,
                                                                    )
                                                                    .child(detail),
                                                            ),
                                                    )
                                            },
                                        )),
                                )
                                .child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .bottom_0()
                                        .left_0()
                                        .right_0()
                                        .child(
                                            gpui_component::scroll::Scrollbar::new(&scroll_handle)
                                                .id("selector-scrollbar")
                                                .axis(gpui_component::scroll::ScrollbarAxis::Vertical)
                                                .scrollbar_show(gpui_component::scroll::ScrollbarShow::Always)
                                        )
                                )
                            )
                        )
                    }
                })
        });
        window.defer(cx, move |window, cx| {
            window.focus(&deferred_selector_focus_handle, cx);
        });
    }

    fn show_transfers_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity();
        window.open_dialog(cx, move |dialog: Dialog, _window, _| {
            dialog
                .title(t!("transfers").to_string())
                .w(px(600.))
                .content({
                    let view = view.clone();
                    move |content, window, cx| {
                        let mut transfers = view.read(cx).transfers.clone();
                        transfers.sort_by_key(|t| {
                            match t.state {
                                crate::terminal::TransferState::Running | crate::terminal::TransferState::Paused => 0,
                                _ => 1,
                            }
                        });

                        if transfers.is_empty() {
                            return content.child(
                                div()
                                    .p_4()
                                    .text_center()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(t!("no_transfers_yet").to_string()),
                            );
                        }

                        let list = v_flex().gap_2().children(transfers.into_iter().map(|t| {
                            let (icon, _color) = match t.info.kind {
                                crate::terminal::TransferType::Upload => (IconName::ArrowUp, cx.theme().primary),
                                crate::terminal::TransferType::Download => (IconName::ArrowDown, cx.theme().success),
                            };

                            let (status_text, actions) = match t.state {
                                crate::terminal::TransferState::Running => {
                                    let percent = t.total.map(|tot| (t.transferred as f64 / tot as f64 * 100.0).clamp(0.0, 100.0)).unwrap_or(0.0);
                                    let txt = if let Some(tot) = t.total {
                                        format!("{:.1}% ({}/{})", percent, format_bytes(t.transferred), format_bytes(tot))
                                    } else {
                                        format!("{}...", t!("downloading"))
                                    };
                                    let btn_pause = Button::new(SharedString::from(format!("pause-{}", t.info.id)))
                                        .ghost()
                                        .small()
                                        .icon(IconName::Pause)
                                        .on_click(window.listener_for(&view, {
                                            let id = t.info.id.clone();
                                            let tab_id = t.tab_id.clone();
                                            move |this, _, _, _| {
                                                if let Some(handle) = this.sftp_handles.get(&tab_id) {
                                                    handle.pause_transfer(id.clone());
                                                }
                                            }
                                        }));
                                    let btn_cancel = Button::new(SharedString::from(format!("cancel-{}", t.info.id)))
                                        .ghost()
                                        .small()
                                        .icon(IconName::Close)
                                        .on_click(window.listener_for(&view, {
                                            let id = t.info.id.clone();
                                            let tab_id = t.tab_id.clone();
                                            move |this, _, _, _| {
                                                if let Some(handle) = this.sftp_handles.get(&tab_id) {
                                                    handle.cancel_transfer(id.clone());
                                                }
                                            }
                                        }));
                                    (txt, h_flex().gap_1().child(btn_pause).child(btn_cancel))
                                }
                                crate::terminal::TransferState::Paused => {
                                    let txt = t!("paused").to_string();
                                    let btn_resume = Button::new(SharedString::from(format!("resume-{}", t.info.id)))
                                        .ghost()
                                        .small()
                                        .icon(IconName::Play)
                                        .on_click(window.listener_for(&view, {
                                            let id = t.info.id.clone();
                                            let tab_id = t.tab_id.clone();
                                            move |this, _, _, _| {
                                                if let Some(handle) = this.sftp_handles.get(&tab_id) {
                                                    handle.resume_transfer(id.clone());
                                                }
                                            }
                                        }));
                                    let btn_cancel = Button::new(SharedString::from(format!("cancel-{}", t.info.id)))
                                        .ghost()
                                        .small()
                                        .icon(IconName::Close)
                                        .on_click(window.listener_for(&view, {
                                            let id = t.info.id.clone();
                                            let tab_id = t.tab_id.clone();
                                            move |this, _, _, _| {
                                                if let Some(handle) = this.sftp_handles.get(&tab_id) {
                                                    handle.cancel_transfer(id.clone());
                                                }
                                            }
                                        }));
                                    (txt, h_flex().gap_1().child(btn_resume).child(btn_cancel))
                                }
                                crate::terminal::TransferState::Completed => {
                                    let txt = t!("completed").to_string();
                                    let mut actions = h_flex().gap_1();
                                    if matches!(t.info.kind, crate::terminal::TransferType::Download) {
                                        let btn_folder = Button::new(SharedString::from(format!("folder-{}", t.info.id)))
                                            .ghost()
                                            .small()
                                            .icon(IconName::Folder)
                                            .on_click({
                                                let target = t.info.target.clone();
                                                move |_, _, _| {
                                                    let _ = std::process::Command::new("open").arg(&target).spawn();
                                                }
                                            });
                                        actions = actions.child(btn_folder);
                                    }
                                    (txt, actions)
                                }
                                crate::terminal::TransferState::Failed(ref err) => {
                                    (format!("{}: {}", t!("failed"), err), h_flex().gap_1())
                                }
                                crate::terminal::TransferState::Cancelled => {
                                    (t!("cancelled").to_string(), h_flex().gap_1())
                                }
                            };

                            let percent = match t.state {
                                crate::terminal::TransferState::Completed => 100.0,
                                _ => t.total.map(|tot| t.transferred as f64 / tot as f64 * 100.0).unwrap_or(0.0),
                            };

                            v_flex()
                                .gap_1()
                                .p_2()
                                .rounded_md()
                                .border_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().muted)
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap_2()
                                        .child(Button::new(SharedString::from(format!("icon-{}", t.info.id))).icon(icon).ghost().small().disabled(true))
                                        .child(
                                            v_flex()
                                                .flex_1()
                                                .min_w(px(0.))
                                                .overflow_hidden()
                                                .child(
                                                    div()
                                                        .text_size(px(12.))
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .text_color(cx.theme().foreground)
                                                        .overflow_hidden()
                                                        .child(t.info.name.clone()),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(px(10.))
                                                        .text_color(cx.theme().muted_foreground)
                                                        .overflow_hidden()
                                                        .child(format!("{}: {}", t!("session"), t.tab_title)),
                                                )
                                        )
                                        .child(
                                            div()
                                                .text_size(px(11.))
                                                .text_color(cx.theme().muted_foreground)
                                                .child(status_text),
                                        )
                                        .child(actions),
                                )
                                .when(matches!(t.state, crate::terminal::TransferState::Running | crate::terminal::TransferState::Paused), |this| {
                                    this.child(
                                        Progress::new(format!("progress-{}", t.info.id))
                                            .with_size(px(4.))
                                            .value(percent as f32)
                                            .color(cx.theme().primary)
                                            .w_full(),
                                    )
                                })
                        }));

                        let scroll_handle = window
                            .use_keyed_state("transfers-scroll", cx, |_, _| gpui::ScrollHandle::default())
                            .read(cx)
                            .clone();

                        content.child(
                            div()
                                .w_full()
                                .relative()
                                .child(
                                    div()
                                        .w_full()
                                        .max_h(px(400.))
                                        .flex_col()
                                        .id("transfers-scroll-view")
                                        .track_scroll(&scroll_handle)
                                        .overflow_y_scroll()
                                        .child(list)
                                )
                                .child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .right_0()
                                        .bottom_0()
                                        .w(px(16.))
                                        .child(
                                            Scrollbar::vertical(&scroll_handle)
                                                .scrollbar_show(ScrollbarShow::Always)
                                        )
                                )
                        )
                    }
                })
        });
    }

    fn show_delete_confirm_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity();
        let selected_entries = self.active_sftp().map(|s| s.selected_entries.clone()).unwrap_or_default();
        if selected_entries.is_empty() {
            return;
        }

        let has_system_path = selected_entries.iter().any(|path| {
            let p = path.as_str();
            p.starts_with("/bin/") || p == "/bin" ||
            p.starts_with("/etc/") || p == "/etc" ||
            p.starts_with("/usr/") || p == "/usr" ||
            p.starts_with("/var/") || p == "/var" ||
            p.starts_with("/sys/") || p == "/sys" ||
            p.starts_with("/dev/") || p == "/dev" ||
            p.starts_with("/boot/") || p == "/boot" ||
            p.starts_with("/lib/") || p == "/lib" ||
            p.starts_with("/opt/") || p == "/opt" ||
            p.starts_with("/run/") || p == "/run" ||
            p.starts_with("/sbin/") || p == "/sbin"
        });

        window.open_dialog(cx, move |dialog: Dialog, _window, _| {
            dialog
                .title(t!("confirm_delete").to_string())
                .w(px(500.))
                .keyboard(false)
                .on_ok({
                    let view = view.clone();
                    let paths_to_delete: Vec<String> = selected_entries.clone().into_iter().collect();
                    move |_, window, cx| {
                        view.update(cx, |this, cx| {
                            if let Some(id) = this.active_tab.clone() {
                                if let Some(handle) = this.sftp_handles.get(&id) {
                                    let _ = handle.commands.send(crate::sftp::SftpCommand::DeletePaths(paths_to_delete.clone()));
                                }
                                if let Some(tab) = this.tabs.iter_mut().find(|t| t.id == id) {
                                    if let Some(sftp) = tab.sftp.as_mut() {
                                        sftp.selected_entries.clear();
                                    }
                                }
                            }
                            cx.notify();
                        });
                        window.close_dialog(cx);
                        true
                    }
                })
                .content({
                    let view = view.clone();
                    move |content, _window, cx| {
                        let scroll_handle = view.read(cx).sftp_delete_scroll_handle.clone();
                        let selected_paths: Vec<String> = view.read(cx)
                            .active_sftp()
                            .map(|s| s.selected_entries.clone().into_iter().collect())
                            .unwrap_or_default();
                        
                        let warning_block = if has_system_path {
                            Some(
                                div()
                                    .w_full()
                                    .p_3()
                                    .mb_3()
                                    .rounded_md()
                                    .bg(gpui::rgba(0xff00001a))
                                    .border_1()
                                    .border_color(gpui::rgba(0xff000080))
                                    .child(
                                        div()
                                            .text_color(gpui::rgba(0xff0000ff))
                                            .font_weight(FontWeight::BOLD)
                                            .child(t!("system_path_warning").to_string())
                                    )
                            )
                        } else {
                            None
                        };

                        let paths_list = div()
                            .relative()
                            .max_h(px(200.))
                            .w_full()
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().background)
                            .rounded_md()
                            .child(
                                v_flex()
                                    .id("delete-scroll-view")
                                    .size_full()
                                    .track_scroll(&scroll_handle)
                                    .overflow_y_scroll()
                                    .p_2()
                                    .gap_1()
                                    .children(selected_paths.into_iter().map(|path| {
                                        div()
                                            .text_size(rems(0.917))
                                            .text_color(cx.theme().muted_foreground)
                                            .child(path)
                                    }))
                            )
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .bottom_0()
                                    .right_0()
                                    .child(
                                        gpui_component::scroll::Scrollbar::vertical(&scroll_handle)
                                            .scrollbar_show(gpui_component::scroll::ScrollbarShow::Always)
                                    )
                            );

                        content.child(
                            v_flex()
                                .w_full()
                                .gap_2()
                                .children(warning_block)
                                .child(
                                    div()
                                        .text_size(rems(1.0))
                                        .mb_2()
                                        .child(t!("confirm_delete_desc", count = view.read(cx).active_sftp().map(|s| s.selected_entries.len()).unwrap_or(0)).to_string())
                                )
                                .child(paths_list)
                        )
                    }
                })
                .footer({
                    let view = view.clone();
                    let paths_to_delete: Vec<String> = selected_entries.clone().into_iter().collect();
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("cancel")
                                .ghost()
                                .label(t!("cancel").to_string())
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                }),
                        )
                        .child(
                            Button::new("confirm")
                                .danger()
                                .label(t!("confirm").to_string())
                                .on_click({
                                    let view = view.clone();
                                    move |_, window, cx| {
                                        view.update(cx, |this, cx| {
                                            if let Some(id) = this.active_tab.clone() {
                                                if let Some(handle) = this.sftp_handles.get(&id) {
                                                    let _ = handle.commands.send(crate::sftp::SftpCommand::DeletePaths(paths_to_delete.clone()));
                                                }
                                                if let Some(tab) = this.tabs.iter_mut().find(|t| t.id == id) {
                                                    if let Some(sftp) = tab.sftp.as_mut() {
                                                        sftp.selected_entries.clear();
                                                    }
                                                }
                                            }
                                            cx.notify();
                                        });
                                        window.close_dialog(cx);
                                    }
                                }),
                        )
                })
        });
    }

    fn show_settings_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity();
        window.open_dialog(cx, move |dialog: Dialog, _window, _| {
            dialog
                .title(t!("settings").to_string())
                .w(px(560.))
                .content({
                    let view = view.clone();
                    move |content, window, cx| {
                        content.child(
                            v_flex()
                                .gap_3()
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap_3()
                                        .child(
                                            div()
                                                .w(px(240.))
                                                .child(t!("ui_font_size").to_string()),
                                        )
                                        .child(
                                            Button::new("ui-font-size-down")
                                                .label("-")
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, _, cx| {
                                                        this.change_ui_font_size(-1.0, cx)
                                                    },
                                                )),
                                        )
                                        .child(div().min_w(px(64.)).text_center().child(format!(
                                            "{:.0}px",
                                            view.read(cx).ui_font_size
                                        )))
                                        .child(
                                            Button::new("ui-font-size-up")
                                                .label("+")
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, _, cx| {
                                                        this.change_ui_font_size(1.0, cx)
                                                    },
                                                )),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap_3()
                                        .child(
                                            div()
                                                .w(px(240.))
                                                .child(t!("terminal_font_size").to_string()),
                                        )
                                        .child(
                                            Button::new("font-size-down")
                                                .label("-")
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, _, cx| {
                                                        this.change_terminal_font_size(-1.0, cx)
                                                    },
                                                )),
                                        )
                                        .child(div().min_w(px(64.)).text_center().child(format!(
                                            "{:.0}px",
                                            view.read(cx).terminal_font_size
                                        )))
                                        .child(
                                            Button::new("font-size-up")
                                                .label("+")
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, _, cx| {
                                                        this.change_terminal_font_size(1.0, cx)
                                                    },
                                                )),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap_3()
                                        .child(
                                            div()
                                                .w(px(240.))
                                                .child(t!("ui_font_family").to_string()),
                                        )
                                        .child(
                                            Button::new("ui-font-dropdown")
                                                .small()
                                                .icon(IconName::ChevronsUpDown)
                                                .label({
                                                    let current = view
                                                        .read(cx)
                                                        .ui_font_family
                                                        .to_string();
                                                    let names = cx.text_system().all_font_names();
                                                    if current == *".SystemUIFont"
                                                        || current.is_empty()
                                                        || !names.contains(&current)
                                                    {
                                                        t!("system_default").to_string()
                                                    } else if current == "Maple Mono NF CN" {
                                                        format!("Maple Mono NF CN ({})", t!("software_builtin"))
                                                    } else {
                                                        current
                                                    }
                                                })
                                                .dropdown_menu_with_anchor(
                                                    Anchor::BottomRight,
                                                    {
                                                        let view = view.clone();
                                                        move |mut menu, window, cx| {
                                                            let current = view
                                                                .read(cx)
                                                                .ui_font_family
                                                                .to_string();
                                                            let mut names =
                                                                cx.text_system().all_font_names();
                                                            menu = menu.min_w(200.).max_h(
                                                                px(320.),
                                                            ).scrollable(true);
                                                            // "System Default" entry
                                                            menu = menu.item(
                                                                PopupMenuItem::new(
                                                                    t!(
                                                                        "system_default"
                                                                    )
                                                                    .to_string(),
                                                                )
                                                                .checked(
                                                                    current == *".SystemUIFont"
                                                                        || current
                                                                            .is_empty(),
                                                                )
                                                                .on_click(window
                                                                    .listener_for(
                                                                    &view,
                                                                    move |this, _, window,
                                                                     cx| {
                                                                        this.change_ui_font_family(
                                                                            ".SystemUIFont",
                                                                            window,
                                                                            cx,
                                                                        );
                                                                    },
                                                                )),
                                                            );
                                                            
                                                            let maple_font = "Maple Mono NF CN".to_string();
                                                            if names.contains(&maple_font) {
                                                                names.retain(|n| n != &maple_font);
                                                                menu = menu.item(
                                                                    PopupMenuItem::new(format!("{} ({})", maple_font, t!("software_builtin")))
                                                                        .checked(current == maple_font)
                                                                        .on_click(window.listener_for(&view, move |this, _, window, cx| {
                                                                            this.change_ui_font_family("Maple Mono NF CN", window, cx);
                                                                        }))
                                                                ).separator();
                                                            }
                                                            
                                                            for name in names {
                                                                let checked =
                                                                    name == current;
                                                                menu = menu.item(
                                                                    PopupMenuItem::new(
                                                                        name.clone(),
                                                                    )
                                                                    .checked(checked)
                                                                    .on_click(window
                                                                        .listener_for(
                                                                        &view,
                                                                        move |this, _,
                                                                         window, cx| {
                                                                            this
                                                                                .change_ui_font_family(
                                                                                &name,
                                                                                window,
                                                                                cx,
                                                                            );
                                                                        },
                                                                    )),
                                                                );
                                                            }
                                                            menu
                                                        }
                                                    },
                                                ),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap_3()
                                        .child(
                                            div()
                                                .w(px(240.))
                                                .child(t!("terminal_font_family").to_string()),
                                        )
                                        .child(
                                            Button::new("terminal-font-dropdown")
                                                .small()
                                                .icon(IconName::ChevronsUpDown)
                                                .label({
                                                    let current = view.read(cx).terminal_font_family.to_string();
                                                    if current == "Maple Mono NF CN" {
                                                        format!("Maple Mono NF CN ({})", t!("software_builtin"))
                                                    } else {
                                                        current
                                                    }
                                                })
                                                .dropdown_menu_with_anchor(
                                                    Anchor::BottomRight,
                                                    {
                                                        let view = view.clone();
                                                        move |mut menu, window, cx| {
                                                            let current = view
                                                                .read(cx)
                                                                .terminal_font_family
                                                                .to_string();
                                                            let mut names =
                                                                cx.text_system().all_font_names();
                                                            menu = menu.min_w(200.).max_h(
                                                                px(320.),
                                                            ).scrollable(true);
                                                            
                                                            let maple_font = "Maple Mono NF CN".to_string();
                                                            if names.contains(&maple_font) {
                                                                names.retain(|n| n != &maple_font);
                                                                menu = menu.item(
                                                                    PopupMenuItem::new(format!("{} ({})", maple_font, t!("software_builtin")))
                                                                        .checked(current == maple_font)
                                                                        .on_click(window.listener_for(&view, move |this, _, _window, cx| {
                                                                            this.change_terminal_font_family("Maple Mono NF CN", cx);
                                                                        }))
                                                                ).separator();
                                                            }

                                                            for name in names {
                                                                let checked =
                                                                    name == current;
                                                                menu = menu.item(
                                                                    PopupMenuItem::new(
                                                                        name.clone(),
                                                                    )
                                                                    .checked(checked)
                                                                    .on_click(window
                                                                        .listener_for(
                                                                        &view,
                                                                        move |this, _,
                                                                         _window, cx| {
                                                                            this
                                                                                .change_terminal_font_family(
                                                                                &name,
                                                                                cx,
                                                                            );
                                                                        },
                                                                    )),
                                                                );
                                                            }
                                                            menu
                                                        }
                                                    },
                                                ),
                                        ),
                                )
                                .child(
                                    v_flex()
                                        .gap_1()
                                        .child(
                                            h_flex()
                                                .items_center()
                                                .gap_3()
                                                .child(div().w(px(240.)).child(t!("right_click_copy_paste").to_string()))
                                                .child(
                                                    Switch::new("right-click-copy-paste")
                                                        .checked(view.read(cx).config.right_click_copy_paste())
                                                        .on_click(window.listener_for(
                                                            &view,
                                                            |this, checked, _, cx| {
                                                                this.config.set_right_click_copy_paste(*checked);
                                                                let _ = this.config.save();
                                                                cx.notify();
                                                            },
                                                        )),
                                                )
                                        )
                                        .child(
                                            div()
                                                .text_size(rems(0.85))
                                                .text_color(cx.theme().muted_foreground)
                                                .child(t!("copy_paste_hint", key = if cfg!(target_os = "macos") { "Command" } else { "Ctrl" }).to_string())
                                        )
                                )
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap_3()
                                        .child(div().w(px(240.)).child(t!("language").to_string()))
                                        .child(
                                            Button::new("language-dropdown")
                                                .small()
                                                .icon(IconName::Globe)
                                                .label({
                                                    let current_locale =
                                                        view.read(cx).config.locale().to_string();
                                                    if current_locale == "en" {
                                                        t!("english").to_string()
                                                    } else if current_locale == "zh-CN" {
                                                        t!("chinese").to_string()
                                                    } else {
                                                        t!("follow_system").to_string()
                                                    }
                                                })
                                                .dropdown_menu_with_anchor(Anchor::BottomRight, {
                                                    let view = view.clone();
                                                    move |mut menu, window, cx| {
                                                        let current_locale = view
                                                            .read(cx)
                                                            .config
                                                            .locale()
                                                            .to_string();
                                                        menu = menu
                                                            .min_w(160.)
                                                            .item(
                                                                PopupMenuItem::new(
                                                                    t!("follow_system").to_string(),
                                                                )
                                                                .checked(current_locale == "system")
                                                                .on_click(window.listener_for(
                                                                    &view,
                                                                    |this, _, window, cx| {
                                                                        this.set_display_language(
                                                                            "system", window, cx,
                                                                        )
                                                                    },
                                                                )),
                                                            )
                                                            .separator()
                                                            .item(
                                                                PopupMenuItem::new(
                                                                    t!("english").to_string(),
                                                                )
                                                                .checked(current_locale == "en")
                                                                .on_click(window.listener_for(
                                                                    &view,
                                                                    |this, _, window, cx| {
                                                                        this.set_display_language(
                                                                            "en", window, cx,
                                                                        )
                                                                    },
                                                                )),
                                                            )
                                                            .item(
                                                                PopupMenuItem::new(
                                                                    t!("chinese").to_string(),
                                                                )
                                                                .checked(current_locale == "zh-CN")
                                                                .on_click(window.listener_for(
                                                                    &view,
                                                                    |this, _, window, cx| {
                                                                        this.set_display_language(
                                                                            "zh-CN", window, cx,
                                                                        )
                                                                    },
                                                                )),
                                                            );
                                                        menu
                                                    }
                                                }),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap_3()
                                        .child(
                                            div().w(px(240.)).child(t!("reset_layout").to_string()),
                                        )
                                        .child(
                                            Button::new("reset-layout")
                                                .label(t!("reset").to_string())
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, window, cx| {
                                                        this.reset_layout(window, cx);
                                                    },
                                                )),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_size(rems(1.0))
                                        .text_color(cx.theme().muted_foreground)
                                        .child(t!("theme_management_hint")),
                                ),
                        )
                    }
                })
        });
    }

    fn reset_layout(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.config.set_layout_state(None, None, None);
        let _ = self.config.save();

        self.workspace_panels = cx.new(|_| gpui_component::resizable::ResizableState::default());
        self.body_panels = cx.new(|_| gpui_component::resizable::ResizableState::default());

        self.status = t!("reset_layout_success").into();
        cx.notify();
    }

    fn set_ssh_auth_method(&mut self, method: AuthMethod, cx: &mut Context<Self>) {
        self.ssh_auth_method = method;
        cx.notify();
    }

    fn connect_saved_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        let Some(session) = self.config.get(&session_id).cloned() else {
            self.status = "saved session not found".into();
            cx.notify();
            return;
        };
        self.open_ssh_session(session, cx);
    }

    fn selector_entries(&self) -> Vec<SelectorEntry> {
        let mut entries = vec![SelectorEntry::Local, SelectorEntry::NewSsh];
        entries.extend(
            self.config
                .sessions()
                .iter()
                .map(|session| SelectorEntry::Saved(session.id.clone())),
        );
        entries
    }

    fn default_selector_index(&self) -> usize {
        if self.config.sessions().is_empty() {
            0
        } else {
            2
        }
    }

    fn move_selector_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
        let entries = self.selector_entries();
        if entries.is_empty() {
            return;
        }
        let current = self.selector_selection.min(entries.len().saturating_sub(1)) as i32;
        let next = (current + delta).clamp(0, entries.len() as i32 - 1) as usize;
        if next != self.selector_selection {
            self.selector_selection = next;
            if next >= 2 {
                self.selector_scroll_handle.scroll_to_item(next - 2);
            }
            cx.notify();
        }
    }

    fn activate_selector_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.selector_entries();
        let Some(entry) = entries.get(self.selector_selection).cloned() else {
            return;
        };

        match entry {
            SelectorEntry::Local => {
                self.open_local(cx);
                window.close_dialog(cx);
            }
            SelectorEntry::NewSsh => {
                window.close_dialog(cx);
                self.open_new_ssh_dialog(window, cx);
            }
            SelectorEntry::Saved(session_id) => {
                self.connect_saved_session(session_id, cx);
                window.close_dialog(cx);
            }
        }
    }

    fn on_selector_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.to_ascii_lowercase();
        match key.as_str() {
            "up" | "arrowup" => {
                self.move_selector_selection(-1, cx);
                window.prevent_default();
                cx.stop_propagation();
            }
            "down" | "arrowdown" => {
                self.move_selector_selection(1, cx);
                window.prevent_default();
                cx.stop_propagation();
            }
            "enter" | "return" => {
                self.activate_selector_selection(window, cx);
                window.prevent_default();
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    fn open_ssh_session(&mut self, session: Session, cx: &mut Context<Self>) {
        let id = Uuid::new_v4().to_string();
        let backend = ssh_terminal::spawn_ssh_terminal(
            self.runtime.handle(),
            id.clone(),
            session.clone(),
            DEFAULT_COLS,
            DEFAULT_ROWS,
            self.events_tx.clone(),
        );
        self.tabs.push(TerminalTab::new_ssh(
            id.clone(),
            &session,
            backend,
            self.events_tx.clone(),
        ));
        self.active_tab = Some(id.clone());
        self.tabs_scroll_handle.scroll_to_item(self.tabs.len() - 1);
        if let Some(session_id) = self.active_session_id() {
            if let Some(index) = self.config.sessions().iter().position(|s| s.id == session_id) {
                self.saved_scroll_handle.scroll_to_item(index);
            }
        }
        cx.notify();
        let sftp_handle = sftp::spawn_sftp(
            self.runtime.handle(),
            id.clone(),
            session,
            self.events_tx.clone(),
        );
        self.sftp_handles.insert(id.clone(), sftp_handle);
        self.active_tab = Some(id.clone());
        self.pending_sftp_path_sync = Some("/".into());
        self.connection_progress = Some(ConnectionProgress {
            tab_id: id,
            title: t!("connecting").into(),
            lines: vec![t!("starting_connection").into()],
            failed: false,
        });
        self.status = "ssh tab opened".into();
        cx.notify();
    }

    fn remove_saved_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.config.remove(&session_id);
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save config: {err:#}");
        }
        self.status = "session removed".into();
        cx.notify();
    }

    fn retry_connection_progress(&mut self, cx: &mut Context<Self>) {
        let Some(progress) = self.connection_progress.clone() else {
            return;
        };
        let Some(ix) = self.tabs.iter().position(|tab| tab.id == progress.tab_id) else {
            self.connection_progress = None;
            cx.notify();
            return;
        };
        let Some(session) = self.tabs[ix].session.clone() else {
            self.connection_progress = None;
            cx.notify();
            return;
        };

        self.tabs[ix].backend.send(BackendCommand::Close);
        if let Some(handle) = self.sftp_handles.remove(&progress.tab_id) {
            handle.close();
        }
        self.tabs.remove(ix);
        if self.active_tab.as_deref() == Some(progress.tab_id.as_str()) {
            self.active_tab = self
                .tabs
                .get(ix)
                .or_else(|| self.tabs.get(ix.saturating_sub(1)))
                .map(|tab| tab.id.clone());
        }
        self.connection_progress = None;
        self.open_ssh_session(session, cx);
    }

    fn cancel_connection_progress(&mut self, cx: &mut Context<Self>) {
        let Some(progress) = self.connection_progress.clone() else {
            return;
        };
        self.connection_progress = None;
        self.close_tab(progress.tab_id, cx);
    }

    fn activate_tab(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self
            .tabs
            .iter()
            .find(|tab| tab.id == id)
            .and_then(|tab| tab.sftp.as_ref())
            .map(|sftp| sftp.current_path.clone())
        {
            self.pending_sftp_path_sync = Some(path);
        }
        self.active_tab = Some(id.clone());
        if let Some(index) = self.tabs.iter().position(|t| t.id == id) {
            self.tabs_scroll_handle.scroll_to_item(index);
        }
        if self.tabs.iter().any(|t| t.id == id) {
            if let Some(session_id) = self.active_session_id() {
                if let Some(index) = self.config.sessions().iter().position(|s| s.id == session_id) {
                    self.saved_scroll_handle.scroll_to_item(index);
                }
            }
        }
        self.remote_sample_in_flight = false;
        self.request_active_system_snapshot();
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn close_tab(&mut self, id: String, cx: &mut Context<Self>) {
        if let Some(ix) = self.tabs.iter().position(|tab| tab.id == id) {
            let was_active = self.active_tab.as_deref() == Some(id.as_str());
            self.tabs[ix].backend.send(BackendCommand::Close);
            if let Some(handle) = self.sftp_handles.remove(&id) {
                handle.close();
            }
            self.tabs.remove(ix);
            if was_active
                || self
                    .active_tab
                    .as_ref()
                    .is_some_and(|active_id| !self.tabs.iter().any(|tab| &tab.id == active_id))
            {
                self.active_tab = self
                    .tabs
                    .get(ix)
                    .or_else(|| self.tabs.get(ix.saturating_sub(1)))
                    .map(|tab| tab.id.clone());
                self.remote_sample_in_flight = false;
                self.request_active_system_snapshot();
            }
            cx.notify();
        }
    }

    fn active_sftp(&self) -> Option<&terminal::SftpUiState> {
        self.active_tab
            .as_ref()
            .and_then(|id| self.tabs.iter().find(|tab| &tab.id == id))
            .and_then(|tab| tab.sftp.as_ref())
    }

    fn active_sftp_mut(&mut self) -> Option<&mut terminal::SftpUiState> {
        let active_id = self.active_tab.clone()?;
        self.tabs
            .iter_mut()
            .find(|tab| tab.id == active_id)
            .and_then(|tab| tab.sftp.as_mut())
    }

    fn active_sftp_handle(&self) -> Option<&SftpHandle> {
        self.active_tab
            .as_ref()
            .and_then(|id| self.sftp_handles.get(id))
    }

    fn navigate_sftp(&mut self, path: String, cx: &mut Context<Self>) {
        if let Some(handle) = self.active_sftp_handle() {
            handle.list_dir(path.clone());
            if let Some(sftp) = self.active_sftp_mut() {
                sftp.current_path = path;
                self.pending_sftp_path_sync = Some(sftp.current_path.clone());
            }
            cx.notify();
        }
    }

    fn select_sftp_entry(&mut self, entry: RemoteEntry, cx: &mut Context<Self>) {
        if entry.is_dir {
            self.navigate_sftp(entry.full_path, cx);
            return;
        }
        self.mark_sftp_entry_selected(&entry.full_path, cx);
        if let Some(sftp) = self.active_sftp_mut() {
            if !sftp.selected_entries.remove(&entry.full_path) {
                sftp.selected_entries.insert(entry.full_path);
            }
        }
    }

    fn mark_sftp_entry_selected(&mut self, path: &str, cx: &mut Context<Self>) {
        if let Some(sftp) = self.active_sftp_mut() {
            sftp.selected_path = Some(path.to_string());
        }
        cx.notify();
    }

    fn sftp_parent_path(path: &str) -> String {
        if path == "/" {
            return "/".to_string();
        }
        path.trim_end_matches('/')
            .rsplit_once('/')
            .map(|(parent, _)| {
                if parent.is_empty() {
                    "/".to_string()
                } else {
                    parent.to_string()
                }
            })
            .unwrap_or_else(|| "/".to_string())
    }

    fn refresh_sftp(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.active_sftp().map(|sftp| sftp.current_path.clone()) {
            self.navigate_sftp(path, cx);
        }
    }

    fn sync_sftp_path_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.pending_sftp_path_sync.take() else {
            return;
        };
        self.sftp_path_input.update(cx, |state, cx| {
            state.set_value(path, window, cx);
        });
    }

    fn open_sftp_context_menu(
        &mut self,
        remote_path: String,
        is_dir: bool,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.sftp_context_menu = Some(SftpContextMenuState {
            remote_path,
            is_dir,
            position,
        });
        cx.notify();
    }

    fn dismiss_sftp_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.sftp_context_menu.take().is_some() {
            cx.notify();
        }
    }

    fn trigger_sftp_context_download(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.sftp_context_menu.take() else {
            return;
        };
        self.download_sftp_entry(menu.remote_path, window, cx);
        cx.notify();
    }

    fn trigger_sftp_context_edit(&mut self, cx: &mut Context<Self>) {
        let Some(menu) = self.sftp_context_menu.take() else {
            return;
        };
        if let Some(id) = self.active_tab.clone() {
            if let Some(handle) = self.sftp_handles.get(&id) {
                handle.edit_file(menu.remote_path);
            }
        }
        cx.notify();
    }

    fn download_sftp_entry(
        &mut self,
        remote_path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(handle) = self.active_sftp_handle().cloned() else {
            return;
        };
        let path_prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Select Download Folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            match path_prompt.await {
                Ok(Ok(Some(mut paths))) => {
                    if let Some(folder) = paths.pop() {
                        handle.download(remote_path, folder.to_string_lossy().to_string());
                    }
                }
                Ok(Err(err)) => {
                    this.update(cx, |this, cx| {
                        this.status = format!("download picker failed: {err}").into();
                        cx.notify();
                    })?;
                }
                _ => {}
            }
            Ok::<(), anyhow::Error>(())
        })
        .detach();
    }

    fn upload_sftp_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(handle) = self.active_sftp_handle().cloned() else {
            return;
        };
        let remote_dir = self
            .active_sftp()
            .map(|sftp| sftp.current_path.clone())
            .unwrap_or_else(|| "/".into());
        let path_prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Select File to Upload".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            match path_prompt.await {
                Ok(Ok(Some(mut paths))) => {
                    if let Some(file) = paths.pop() {
                        handle.upload_paths(vec![file.to_string_lossy().to_string()], remote_dir);
                    }
                }
                Ok(Err(err)) => {
                    this.update(cx, |this, cx| {
                        this.status = format!("upload picker failed: {err}").into();
                        cx.notify();
                    })?;
                }
                _ => {}
            }
            Ok::<(), anyhow::Error>(())
        })
        .detach();
    }

    fn upload_sftp_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(handle) = self.active_sftp_handle().cloned() else {
            return;
        };
        let remote_dir = self
            .active_sftp()
            .map(|sftp| sftp.current_path.clone())
            .unwrap_or_else(|| "/".into());
        let path_prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Select Folder to Upload".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            match path_prompt.await {
                Ok(Ok(Some(mut paths))) => {
                    if let Some(folder) = paths.pop() {
                        handle.upload_paths(vec![folder.to_string_lossy().to_string()], remote_dir);
                    }
                }
                Ok(Err(err)) => {
                    this.update(cx, |this, cx| {
                        this.status = format!("upload picker failed: {err}").into();
                        cx.notify();
                    })?;
                }
                _ => {}
            }
            Ok::<(), anyhow::Error>(())
        })
        .detach();
    }

    fn switch_theme_mode(&mut self, mode: ThemeMode, window: &mut Window, cx: &mut Context<Self>) {
        self.follow_system_theme = false;
        self.theme_mode = mode;
        self.apply_theme_preferences(window, cx);
        self.status = format!("theme mode: {}", cx.theme().mode.name()).into();
        self.persist_theme_preferences();
        cx.notify();
    }

    fn apply_theme(&mut self, name: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let Some(theme_config) = ThemeRegistry::global(cx).themes().get(&name).cloned() else {
            self.status = format!("theme not found: {name}").into();
            cx.notify();
            return;
        };

        if theme_config.mode.is_dark() {
            self.dark_theme_name = name.clone();
        } else {
            self.light_theme_name = name.clone();
        }
        self.apply_theme_preferences(window, cx);
        self.status = format!("theme: {name}").into();
        self.persist_theme_preferences();
        window.refresh();
        cx.notify();
    }

    fn set_follow_system_theme(
        &mut self,
        follow: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.follow_system_theme = follow;
        if follow {
            self.status = "theme mode: system".into();
        } else {
            self.status = format!("theme mode: {}", cx.theme().mode.name()).into();
        }
        self.apply_theme_preferences(window, cx);
        self.persist_theme_preferences();
        cx.notify();
    }

    fn set_display_language(&mut self, locale: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.config.set_locale(locale);
        let mut active_locale = locale.to_string();
        if active_locale == "system" {
            active_locale = sys_locale::get_locale().unwrap_or_else(|| "en".to_string());
            if active_locale.starts_with("zh") {
                active_locale = "zh-CN".to_string();
            } else {
                active_locale = "en".to_string();
            }
        }
        rust_i18n::set_locale(&active_locale);
        gpui_component::set_locale(&active_locale);
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save language preferences: {err:#}");
        }
        window.refresh();
        cx.notify();
    }

    fn apply_theme_preferences(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let light_theme = ThemeRegistry::global(cx)
            .themes()
            .get(&self.light_theme_name)
            .cloned()
            .unwrap_or_else(|| ThemeRegistry::global(cx).default_light_theme().clone());
        let dark_theme = ThemeRegistry::global(cx)
            .themes()
            .get(&self.dark_theme_name)
            .cloned()
            .unwrap_or_else(|| ThemeRegistry::global(cx).default_dark_theme().clone());
        let theme = Theme::global_mut(cx);
        theme.light_theme = light_theme;
        theme.dark_theme = dark_theme;
        theme.font_size = px(self.ui_font_size);
        set_theme_font_names(theme, &self.ui_font_family);

        if self.follow_system_theme {
            Theme::sync_system_appearance(Some(window), cx);
        } else {
            Theme::change(self.theme_mode, Some(window), cx);
        }
    }

    fn persist_theme_preferences(&mut self) {
        let theme_mode_str = match self.theme_mode {
            ThemeMode::Light => "light",
            ThemeMode::Dark => "dark",
        };
        self.config.set_theme_preferences(
            self.follow_system_theme,
            theme_mode_str,
            self.light_theme_name.to_string(),
            self.dark_theme_name.to_string(),
        );
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save theme preferences: {err:#}");
        }
    }

    fn on_terminal_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.modifiers.secondary() && event.keystroke.key == "," {
            self.show_settings_dialog(window, cx);
            window.prevent_default();
            cx.stop_propagation();
            return;
        }
        if event.keystroke.modifiers.shift
            && event.keystroke.modifiers.secondary()
            && event.keystroke.key == "o"
        {
            self.show_selector_dialog(window, cx);
            window.prevent_default();
            cx.stop_propagation();
            return;
        }
        if event.keystroke.modifiers.secondary() && event.keystroke.key.eq_ignore_ascii_case("c") {
            if let Some(text) = self.active_terminal_selection_text() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                window.prevent_default();
                cx.stop_propagation();
                return;
            }
        }
        if event.keystroke.modifiers.secondary() && event.keystroke.key.eq_ignore_ascii_case("v") {
            if let Some(clipboard) = cx.read_from_clipboard() {
                if let Some(text) = clipboard.text() {
                    self.paste_into_terminal(&text, window, cx);
                    return;
                }
            }
        }

        if event.prefer_character_input {
            if let Some(text) = event.keystroke.key_char.as_deref() {
                if !text.is_empty()
                    && !event.keystroke.modifiers.control
                    && !event.keystroke.modifiers.function
                    && !event.keystroke.modifiers.platform
                {
                    self.send_terminal_input(text.as_bytes().to_vec(), window, cx);
                }
            }
            return;
        }

        let Some(active_id) = self.active_tab.clone() else {
            return;
        };
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == active_id) else {
            return;
        };

        if tab.render_snapshot().display_offset > 0 {
            tab.scroll_to_bottom();
        }
        tab.clear_selection();

        if let Some(bytes) = encode_key(&event.keystroke, tab.app_cursor_mode(), false) {
            tab.backend.send(BackendCommand::Input(bytes));
            window.prevent_default();
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn on_terminal_tab_action(
        &mut self,
        _: &TerminalTabKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.send_terminal_input(vec![b'\t'], window, cx);
    }

    fn on_terminal_backtab_action(
        &mut self,
        _: &TerminalBacktabKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.send_terminal_input(b"\x1b[Z".to_vec(), window, cx);
    }

    fn send_terminal_input(&mut self, bytes: Vec<u8>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active_id) = self.active_tab.clone() else {
            return;
        };
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == active_id) else {
            return;
        };

        if tab.render_snapshot().display_offset > 0 {
            tab.scroll_to_bottom();
        }

        tab.clear_selection();
        tab.backend.send(BackendCommand::Input(bytes));
        window.prevent_default();
        cx.stop_propagation();
        cx.notify();
    }

    fn active_terminal_selection_text(&self) -> Option<String> {
        let active_id = self.active_tab.as_ref()?;
        self.tabs
            .iter()
            .find(|tab| &tab.id == active_id)
            .and_then(|tab| tab.selection_text())
    }

    fn paste_into_terminal(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active_id) = self.active_tab.clone() else {
            return;
        };
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == active_id) else {
            return;
        };

        if tab.render_snapshot().display_offset > 0 {
            tab.scroll_to_bottom();
        }
        tab.clear_selection();
        tab.paste_text(text);
        window.prevent_default();
        cx.stop_propagation();
        cx.notify();
    }

    pub(crate) fn terminal_accepts_text_input(&self) -> bool {
        self.active_tab.is_some()
    }

    pub(crate) fn terminal_marked_text_range(&self) -> Option<Range<usize>> {
        self.terminal_marked_text
            .as_ref()
            .map(|text| 0..text.encode_utf16().count())
    }

    pub(crate) fn set_terminal_marked_text(
        &mut self,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.terminal_marked_text = if text.is_empty() { None } else { Some(text) };
        window.invalidate_character_coordinates();
        cx.notify();
    }

    pub(crate) fn clear_terminal_marked_text(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.terminal_marked_text.take().is_some() {
            window.invalidate_character_coordinates();
            cx.notify();
        }
    }

    pub(crate) fn commit_terminal_ime_text(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(active_id) = self.active_tab.clone() else {
            return;
        };
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == active_id) else {
            return;
        };

        if tab.render_snapshot().display_offset > 0 {
            tab.scroll_to_bottom();
        }
        tab.clear_selection();
        self.terminal_marked_text = None;
        tab.backend
            .send(BackendCommand::Input(text.as_bytes().to_vec()));
        window.invalidate_character_coordinates();
        cx.notify();
    }

    pub(crate) fn terminal_ime_bounds_for_range(
        &self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
    ) -> Option<Bounds<Pixels>> {
        let snapshot = self.active_snapshot()?;
        let cursor = snapshot.cursor?;
        let x = element_bounds.origin.x
            + px(self.terminal_cell_width()) * cursor.col as f32
            + px(self.terminal_cell_width()) * range_utf16.start as f32;
        let y = element_bounds.origin.y + px(self.terminal_line_height()) * cursor.row as f32;
        Some(Bounds::new(
            point(x, y),
            size(
                px(self.terminal_cell_width()),
                px(self.terminal_line_height()),
            ),
        ))
    }

    fn focus_terminal(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window, cx);
        if event.button == MouseButton::Left {
            self.begin_terminal_selection(event, cx);
        }
        cx.notify();
    }

    fn on_terminal_right_click(
        &mut self,
        _event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.config.right_click_copy_paste() {
            return;
        }

        let mut handled = false;
        if let Some(text) = self.active_terminal_selection_text() {
            if !text.is_empty() {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                
                let active_id = self.active_tab.clone();
                if let Some(active_id) = active_id {
                    if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == active_id) {
                        tab.clear_selection();
                    }
                }
                cx.notify();
                handled = true;
            }
        }
        
        if !handled {
            if let Some(clipboard_item) = cx.read_from_clipboard() {
                if let Some(text) = clipboard_item.text() {
                    if !text.is_empty() {
                        self.paste_into_terminal(&text, window, cx);
                    }
                }
            }
        }
    }

    fn begin_terminal_selection(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let click_count = event.click_count.max(1);
        let selection_type = match click_count {
            1 => SelectionType::Simple,
            2 => SelectionType::Semantic,
            3 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        let Some((row, col, side)) = self.terminal_grid_point_and_side(event.position) else {
            return;
        };
        let Some(active_id) = self.active_tab.clone() else {
            return;
        };
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == active_id) {
            tab.begin_selection(row, col, side, selection_type);
            self.terminal_selecting = true;
            cx.notify();
        }
    }

    fn on_terminal_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.terminal_selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let Some((row, col, side)) = self.terminal_grid_point_and_side(event.position) else {
            return;
        };
        let Some(active_id) = self.active_tab.clone() else {
            return;
        };
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == active_id) {
            tab.update_selection(row, col, side);
            cx.notify();
        }
    }

    fn on_terminal_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.terminal_selecting = false;
        cx.notify();
    }

    fn terminal_grid_point_and_side(
        &self,
        position: Point<Pixels>,
    ) -> Option<(usize, usize, Side)> {
        let bounds = self.terminal_bounds?;
        if !bounds.contains(&position) {
            return None;
        }
        let local_x = (position.x - bounds.origin.x).max(px(0.));
        let local_y = (position.y - bounds.origin.y).max(px(0.));
        let cell_width = px(self.terminal_cell_width());
        let line_height = px(self.terminal_line_height());
        let snapshot = self.active_snapshot()?;
        let max_col = snapshot.cols.saturating_sub(1);
        let max_row = snapshot.rows.saturating_sub(1);
        let col = ((local_x / cell_width).floor() as usize).min(max_col);
        let row = ((local_y / line_height).floor() as usize).min(max_row);
        let cell_offset_x = px(local_x.as_f32() % cell_width.as_f32());
        let side = if cell_offset_x >= (cell_width / 2.) {
            Side::Right
        } else {
            Side::Left
        };
        Some((row, col, side))
    }

    fn on_terminal_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta_lines = match event.delta {
            ScrollDelta::Lines(point) => point.y.round() as i32,
            ScrollDelta::Pixels(point) => {
                (point.y.as_f32() / self.terminal_line_height()).round() as i32
            }
        };
        if delta_lines == 0 {
            return;
        }
        let Some(active_id) = self.active_tab.clone() else {
            return;
        };
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == active_id) {
            tab.scroll_history(delta_lines);
            window.prevent_default();
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn active_snapshot(&self) -> Option<terminal::RenderSnapshot> {
        self.active_tab
            .as_ref()
            .and_then(|id| self.tabs.iter().find(|t| &t.id == id))
            .map(TerminalTab::render_snapshot)
    }

    fn render_home_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .h_full()
            .items_center()
            .justify_center()
            .gap_4()
            .child(
                div()
                    .text_size(rems(2.333))
                    .font_weight(FontWeight::BOLD)
                    .child("ashell"),
            )
            .child(
                div()
                    .text_size(rems(1.083))
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("open_local_or_ssh")),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        Button::new("home-open-local")
                            .primary()
                            .label(t!("local_terminal").to_string())
                            .on_click(cx.listener(|this, _, _, cx| this.open_local(cx))),
                    )
                    .child(
                        Button::new("home-open-session")
                            .ghost()
                            .label(t!("open_session").to_string())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.show_selector_dialog(window, cx)
                            })),
                    ),
            )
    }

    fn active_status(&self) -> String {
        self.active_tab
            .as_ref()
            .and_then(|id| self.tabs.iter().find(|t| &t.id == id))
            .map(|t| t.status.clone())
            .unwrap_or_else(|| self.status.to_string())
    }

    fn active_connection_ok(&self) -> bool {
        let status = self.active_status().to_ascii_lowercase();
        let has_error = ["failed", "error", "closed", "unavailable", "required"]
            .iter()
            .any(|needle| status.contains(needle));
        !has_error && self.active_tab.is_some()
    }

    fn active_connection_color(&self, cx: &mut Context<Self>) -> Hsla {
        if self.active_connection_ok() {
            cx.theme().success
        } else {
            cx.theme().danger
        }
    }

    fn active_kind(&self) -> Option<TabKind> {
        self.active_tab
            .as_ref()
            .and_then(|id| self.tabs.iter().find(|t| &t.id == id))
            .map(|tab| tab.kind)
    }

    fn active_title(&self) -> String {
        self.active_tab
            .as_ref()
            .and_then(|id| self.tabs.iter().find(|t| &t.id == id))
            .map(|t| t.title.clone())
            .unwrap_or_else(|| t!("idle_no_session").into())
    }

    fn active_ssh_session(&self) -> Option<(String, Session)> {
        let active_id = self.active_tab.as_ref()?;
        let tab = self.tabs.iter().find(|tab| &tab.id == active_id)?;
        if !tab.connected {
            return None;
        }
        Some((tab.id.clone(), tab.session.clone()?))
    }

    fn active_session_id(&self) -> Option<&str> {
        self.active_tab
            .as_ref()
            .and_then(|id| self.tabs.iter().find(|tab| &tab.id == id))
            .and_then(|tab| tab.session.as_ref())
            .map(|session| session.id.as_str())
    }

    fn session_detail(&self, session: &Session) -> String {
        format!("{}@{}:{}", session.user, session.host, session.port)
    }

    fn system_target_label(&self) -> String {
        match self.active_kind() {
            Some(TabKind::Ssh) => self.active_title(),
            Some(TabKind::Local) => "local host".into(),
            None => "idle".into(),
        }
    }

    fn sync_terminal_size(&mut self, window: &Window, cx: &App) {
        let viewport = window.viewport_size();
        let sidebar_width = self
            .workspace_panels
            .read(cx)
            .sizes()
            .first()
            .map(|size| size.as_f32())
            .unwrap_or(SIDEBAR_WIDTH);
        let terminal_height = self
            .body_panels
            .read(cx)
            .sizes()
            .first()
            .map(|size| size.as_f32())
            .unwrap_or(viewport.height.as_f32() - TAB_BAR_HEIGHT - 248.0);
        let width = (viewport.width.as_f32() - sidebar_width - TERMINAL_PADDING_X - 8.0)
            .max(self.terminal_cell_width());
        let height = (terminal_height - TERMINAL_PADDING_Y).max(self.terminal_line_height());
        let cols = (width / self.terminal_cell_width()).floor().max(1.0) as u16;
        let rows = (height / self.terminal_line_height()).floor().max(1.0) as u16;

        for tab in &mut self.tabs {
            tab.resize(cols, rows);
        }
    }

    fn system_card(
        &self,
        label: String,
        percent: f32,
        detail: String,
        fill: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let percent = percent.clamp(0.0, 1.0);
        v_flex()
            .gap_1()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(rems(0.917))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(fill)
                            .child(label.clone()),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(rems(0.917))
                            .text_color(cx.theme().muted_foreground)
                            .flex_none()
                            .child(format!("{:.0}%", percent * 100.0)),
                    ),
            )
            .child(
                Progress::new(format!("system-progress-{label}"))
                    .with_size(px(6.))
                    .value(percent * 100.0)
                    .color(fill)
                    .w_full(),
            )
            .child(
                div()
                    .w_full()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_size(rems(0.917))
                    .text_color(cx.theme().muted_foreground)
                    .child(detail),
            )
    }

    fn network_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_2()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(rems(1.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().chart_4)
                            .child(t!("net")),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(rems(0.917))
                            .text_color(cx.theme().muted_foreground)
                            .child(t!("live")),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w(px(0.))
                            .gap_1()
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(rems(1.0))
                                    .text_color(cx.theme().chart_4)
                                    .child("↓"),
                            )
                            .child(div().text_size(rems(1.0)).child(self.system.net_rx.clone())),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w(px(0.))
                            .gap_1()
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(rems(1.0))
                                    .text_color(cx.theme().chart_5)
                                    .child("↑"),
                            )
                            .child(div().text_size(rems(1.0)).child(self.system.net_tx.clone())),
                    ),
            )
    }

    fn disk_row(&self, disk: DiskSample, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mount = disk.mount.clone();
        let used = disk.total_bytes.saturating_sub(disk.available_bytes);
        let percent = if disk.total_bytes == 0 {
            0.0
        } else {
            used as f32 / disk.total_bytes as f32
        }
        .clamp(0.0, 1.0);

        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .text_size(rems(0.917))
                            .text_color(cx.theme().foreground)
                            .child(mount.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(rems(0.917))
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{:.0}%", percent * 100.0)),
                    ),
            )
            .child(
                Progress::new(format!("disk-progress-{mount}"))
                    .with_size(px(5.))
                    .value(percent * 100.0)
                    .color(cx.theme().chart_5)
                    .w_full(),
            )
            .child(
                div()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_size(rems(0.917))
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{}/{}",
                        format_bytes(used),
                        format_bytes(disk.total_bytes)
                    )),
            )
    }

    fn toggle_sftp_entry(&mut self, path: String, checked: bool, cx: &mut Context<Self>) {
        if let Some(sftp) = self.active_sftp_mut() {
            if checked {
                sftp.selected_entries.insert(path);
            } else {
                sftp.selected_entries.remove(&path);
            }
            cx.notify();
        }
    }

    fn toggle_all_sftp_entries(&mut self, checked: bool, cx: &mut Context<Self>) {
        if let Some(sftp) = self.active_sftp_mut() {
            if checked {
                let paths: Vec<String> = sftp.entries.iter().map(|e| e.full_path.clone()).collect();
                for path in paths {
                    sftp.selected_entries.insert(path);
                }
            } else {
                sftp.selected_entries.clear();
            }
            cx.notify();
        }
    }

    fn download_selected_sftp_entries(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sftp) = self.active_sftp() else {
            return;
        };
        let selected: Vec<String> = sftp.selected_entries.iter().cloned().collect();
        if selected.is_empty() {
            return;
        }

        let Some(handle) = self.active_sftp_handle().cloned() else {
            return;
        };

        let path_prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Select Download Folder".into()),
        });

        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(mut paths))) = path_prompt.await {
                if let Some(folder) = paths.pop() {
                    let local_dir = folder.to_string_lossy().to_string();
                    for remote in selected {
                        let _ = handle.commands.send(crate::sftp::SftpCommand::Download {
                            remote,
                            local_dir: local_dir.clone(),
                        });
                    }

                    let _ = this.update(cx, |this, cx| {
                        if let Some(sftp_mut) = this.active_sftp_mut() {
                            sftp_mut.selected_entries.clear();
                        }
                        cx.notify();
                    });
                }
            }
            Ok::<(), anyhow::Error>(())
        })
        .detach();
    }

    fn upload_sftp_files_batch(&mut self, paths: Vec<String>, _cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        if let Some(sftp) = self.active_sftp() {
            if let Some(handle) = self.active_sftp_handle() {
                let _ = handle.commands.send(crate::sftp::SftpCommand::UploadPaths {
                    locals: paths,
                    remote_dir: sftp.current_path.clone(),
                });
            }
        }
    }

    fn theme_dropdown(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let themes = ThemeRegistry::global(cx)
            .sorted_themes()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let light_themes = themes
            .iter()
            .filter(|theme| !theme.mode.is_dark())
            .map(|theme| theme.name.clone())
            .collect::<Vec<_>>();
        let dark_themes = themes
            .iter()
            .filter(|theme| theme.mode.is_dark())
            .map(|theme| theme.name.clone())
            .collect::<Vec<_>>();
        let follow_system = self.follow_system_theme;
        let is_dark_mode = cx.theme().mode.is_dark();
        let light_theme_name = self.light_theme_name.clone();
        let dark_theme_name = self.dark_theme_name.clone();
        let icon = if follow_system {
            IconName::Sun
        } else if is_dark_mode {
            IconName::Moon
        } else {
            IconName::Sun
        };

        Button::new("theme-dropdown")
            .ghost()
            .small()
            .icon(icon)
            .dropdown_menu_with_anchor(Anchor::BottomRight, move |mut menu, window, _| {
                menu = menu
                    .min_w(220.)
                    .item(
                        PopupMenuItem::new(t!("follow_system"))
                            .checked(follow_system)
                            .on_click(window.listener_for(&view, |this, _, window, cx| {
                                this.set_follow_system_theme(true, window, cx)
                            })),
                    )
                    .item(
                        PopupMenuItem::new(t!("use_light_mode"))
                            .checked(!follow_system && !is_dark_mode)
                            .on_click(window.listener_for(&view, |this, _, window, cx| {
                                this.switch_theme_mode(ThemeMode::Light, window, cx)
                            })),
                    )
                    .item(
                        PopupMenuItem::new(t!("use_dark_mode"))
                            .checked(!follow_system && is_dark_mode)
                            .on_click(window.listener_for(&view, |this, _, window, cx| {
                                this.switch_theme_mode(ThemeMode::Dark, window, cx)
                            })),
                    )
                    .separator()
                    .label(t!("light_theme").to_string());

                for theme_name in light_themes.clone() {
                    let checked = theme_name == light_theme_name;
                    menu = menu.item(
                        PopupMenuItem::new(theme_name.clone())
                            .checked(checked)
                            .on_click(window.listener_for(&view, move |this, _, window, cx| {
                                this.apply_theme(theme_name.clone(), window, cx)
                            })),
                    );
                }

                menu = menu.separator();
                menu = menu.label(t!("dark_theme").to_string());

                for theme_name in dark_themes.clone() {
                    let checked = theme_name == dark_theme_name;
                    menu = menu.item(
                        PopupMenuItem::new(theme_name.clone())
                            .checked(checked)
                            .on_click(window.listener_for(&view, move |this, _, window, cx| {
                                this.apply_theme(theme_name.clone(), window, cx)
                            })),
                    );
                }

                menu
            })
    }

    fn render_sftp_panel(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active_sftp = self.active_sftp();
        
        let header = h_flex()
            .flex_none()
            .h(px(34.))
            .items_center()
            .gap_2()
            .px_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().tab_bar)
            .child(
                div()
                    .text_size(rems(1.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().primary)
                    .child(t!("remote_files")),
            )
            .child(div().flex_1())
            .when_some(active_sftp.clone(), |this, sftp| {
                let selected_entries = sftp.selected_entries.clone();
                this.child(
                    Button::new("sftp-refresh")
                        .ghost()
                        .small()
                        .icon(IconName::ArrowRight)
                        .label(t!("refresh").to_string())
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_sftp(cx))),
                )
                .child(
                    Button::new("sftp-new-folder")
                        .ghost()
                        .small()
                        .icon(IconName::Folder)
                        .label(t!("new_folder").to_string())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.sftp_creating_folder = true;
                            this.sftp_new_folder_input.update(cx, |input, cx| {
                                input.set_value("", window, cx);
                                input.focus_handle(cx).focus(window, cx);
                            });
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("sftp-delete-selected")
                        .ghost()
                        .small()
                        .icon(IconName::Close)
                        .label(if selected_entries.is_empty() {
                            t!("delete_selected").to_string()
                        } else {
                            format!("{} ({})", t!("delete_selected").to_string(), selected_entries.len())
                        })
                        .disabled(selected_entries.is_empty())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.show_delete_confirm_dialog(window, cx);
                        })),
                )
                .child(
                    Button::new("sftp-upload-file")
                        .ghost()
                        .small()
                        .icon(IconName::Plus)
                        .label(t!("upload_file").to_string())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.upload_sftp_files(window, cx)
                        })),
                )
                .child(
                    Button::new("sftp-upload-folder")
                        .ghost()
                        .small()
                        .icon(IconName::Folder)
                        .label(t!("upload_folder").to_string())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.upload_sftp_folder(window, cx)
                        })),
                )
                .child(
                    Button::new("sftp-download-selected")
                        .ghost()
                        .small()
                        .icon(IconName::ArrowDown)
                        .label(if selected_entries.is_empty() {
                            t!("download").to_string()
                        } else {
                            t!("download_count", count = selected_entries.len()).to_string()
                        })
                        .disabled(selected_entries.is_empty())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.download_selected_sftp_entries(window, cx);
                        })),
                )
                .child(
                    Checkbox::new("sftp-show-hidden")
                        .small()
                        .label(t!("hidden").to_string())
                        .checked(self.show_hidden_files)
                        .tab_stop(false)
                        .on_click(cx.listener(|this, checked, _, cx| {
                            this.show_hidden_files = *checked;
                            cx.notify();
                        })),
                )
            })
            .child(
                Button::new("open-transfers")
                    .ghost()
                    .small()
                    .icon(IconName::ArrowDown)
                    .label(t!("transfers").to_string())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_transfers_dialog(window, cx);
                    })),
            );

        let Some(sftp) = active_sftp else {
            return v_flex()
                .size_full()
                .gap_0()
                .border_color(cx.theme().border)
                .bg(cx.theme().background)
                .child(header)
                .child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .p_3()
                        .child(
                            div()
                                .text_size(rems(1.0))
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("open_ssh_tab_sftp")),
                        ),
                )
                .into_any_element();
        };

        let selected_path = sftp.selected_path.clone();
        let entries = sftp
            .entries
            .clone()
            .into_iter()
            .filter(|entry| self.show_hidden_files || !entry.name.starts_with('.'))
            .collect::<Vec<_>>();
        let status = sftp.status.clone();
        let selected_entries = sftp.selected_entries.clone();
        let all_selected = !entries.is_empty()
            && entries
                .iter()
                .all(|e| selected_entries.contains(&e.full_path));
        let parent_path = Self::sftp_parent_path(&sftp.current_path);
        let view = cx.entity();
        let icon_col_width = px(14.);
        let size_col_width = px(96.);
        let modified_col_width = px(152.);

        v_flex()
            .size_full()
            .gap_0()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .on_drop(
                cx.listener(|this, paths: &gpui::ExternalPaths, _window, cx| {
                    let paths_to_upload: Vec<String> = paths
                        .0
                        .iter()
                        .map(|p| p.to_string_lossy().to_string())
                        .collect();
                    this.upload_sftp_files_batch(paths_to_upload, cx);
                }),
            )
            .child(header)
            .child(
                h_flex()
                    .h(px(36.))
                    .items_center()
                    .gap_2()
                    .px_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted)
                    .child(
                        Button::new("sftp-up")
                            .ghost()
                            .small()
                            .icon(IconName::ChevronUp)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.navigate_sftp(parent_path.clone(), cx);
                            })),
                    )
                    .child(Input::new(&self.sftp_path_input).flex_1().tab_index(0))
                    .child(div().flex_none()),
            )
            .child(
                h_flex()
                    .h(px(26.))
                    .px_3()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted.opacity(0.8))
                    .child(
                        h_flex()
                            .w(px(24.))
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .child(
                                Checkbox::new("sftp-select-all")
                                    .checked(all_selected)
                                    .on_click(cx.listener(move |this, checked, _, cx| {
                                        this.toggle_all_sftp_entries(*checked, cx);
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w(px(0.))
                            .items_center()
                            .gap_2()
                            .child(div().w(icon_col_width).flex_none())
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(rems(0.917))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(t!("name")),
                            ),
                    )
                    .child(
                        div()
                            .w(size_col_width)
                            .flex_none()
                            .text_size(rems(0.917))
                            .text_color(cx.theme().muted_foreground)
                            .child(t!("size")),
                    )
                    .child(
                        div()
                            .w(modified_col_width)
                            .flex_none()
                            .text_size(rems(0.917))
                            .text_color(cx.theme().muted_foreground)
                            .child(t!("modified")),
                    )
                    .child(div().w(px(12.)).flex_none()),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .w_full()
                    .relative()
                    .child(
                        uniform_list("remote-files-list", entries.len() + if self.sftp_creating_folder { 1 } else { 0 }, {
                            let entries = entries.clone();
                            let selected_path = selected_path.clone();
                            let view = view.clone();
                            let creating_folder = self.sftp_creating_folder;
                            let new_folder_input = self.sftp_new_folder_input.clone();
                            move |visible_range, window, cx| {
                                visible_range
                                    .map(|ix| {
                                        if creating_folder && ix == 0 {
                                            return h_flex()
                                                .w_full()
                                                .h(px(28.))
                                                .items_center()
                                                .gap_2()
                                                .px_3()
                                                .bg(cx.theme().tab_active)
                                                .border_b_1()
                                                .border_color(cx.theme().border.opacity(0.35))
                                                .child(
                                                    h_flex()
                                                        .w(px(24.))
                                                        .flex_none()
                                                        .items_center()
                                                        .justify_center(),
                                                )
                                                .child(
                                                    div()
                                                        .w(px(24.))
                                                        .flex_none()
                                                        .text_size(rems(1.0))
                                                        .child("📁"),
                                                )
                                                .child(
                                                    gpui_component::input::Input::new(&new_folder_input)
                                                        .flex_1()
                                                )
                                                .into_any_element();
                                        }

                                        let actual_ix = if creating_folder { ix - 1 } else { ix };
                                        let entry = entries[actual_ix].clone();
                                        let left_row = entry.clone();
                                        let right_row = entry.clone();
                                        let remote_path = entry.full_path.clone();
                                        let is_checked =
                                            selected_entries.contains(&entry.full_path);
                                        let is_selected = selected_path
                                            .as_deref()
                                            .map(|path| path == entry.full_path)
                                            .unwrap_or(false);
                                        let bg = if is_selected {
                                            cx.theme().tab_active
                                        } else if ix % 2 == 0 {
                                            cx.theme().background
                                        } else {
                                            cx.theme().muted.opacity(0.45)
                                        };
                                        let name_color = if entry.is_dir {
                                            cx.theme().primary
                                        } else {
                                            cx.theme().foreground
                                        };
                                        h_flex()
                                            .w_full()
                                            .h(px(28.))
                                            .items_center()
                                            .gap_2()
                                            .px_3()
                                            .bg(bg)
                                            .hover(|style| style.bg(cx.theme().muted.opacity(0.8)))
                                            .border_b_1()
                                            .border_color(cx.theme().border.opacity(0.35))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                window.listener_for(
                                                    &view,
                                                    move |this, _, _, cx| {
                                                        this.dismiss_sftp_context_menu(cx);
                                                        this.select_sftp_entry(
                                                            left_row.clone(),
                                                            cx,
                                                        );
                                                    },
                                                ),
                                            )
                                            .on_mouse_down(
                                                MouseButton::Right,
                                                window.listener_for(&view, {
                                                    let remote_path = remote_path.clone();
                                                    move |this, event: &MouseDownEvent, _, cx| {
                                                        this.mark_sftp_entry_selected(
                                                            &right_row.full_path,
                                                            cx,
                                                        );
                                                        this.open_sftp_context_menu(
                                                            remote_path.clone(),
                                                            entry.is_dir,
                                                            event.position,
                                                            cx,
                                                        );
                                                    }
                                                }),
                                            )
                                            .child(
                                                h_flex()
                                                    .w(px(24.))
                                                    .flex_none()
                                                    .items_center()
                                                    .justify_center()
                                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                        cx.stop_propagation()
                                                    })
                                                    .on_mouse_down(
                                                        MouseButton::Right,
                                                        |_, _, cx| cx.stop_propagation(),
                                                    )
                                                    .child(
                                                        Checkbox::new(ElementId::Name(
                                                            format!("check-{}", entry.full_path)
                                                                .into(),
                                                        ))
                                                        .checked(is_checked)
                                                        .on_click(window.listener_for(&view, {
                                                            let path = entry.full_path.clone();
                                                            move |this, checked, _, cx| {
                                                                this.toggle_sftp_entry(
                                                                    path.clone(),
                                                                    *checked,
                                                                    cx,
                                                                );
                                                            }
                                                        })),
                                                    ),
                                            )
                                            .child(
                                                h_flex()
                                                    .flex_1()
                                                    .min_w(px(0.))
                                                    .items_center()
                                                    .gap_2()
                                                    .child(
                                                        div()
                                                            .w(icon_col_width)
                                                            .flex_none()
                                                            .text_size(rems(1.0))
                                                            .text_color(name_color)
                                                            .child(if entry.is_dir {
                                                                "📁"
                                                            } else {
                                                                "📄"
                                                            }),
                                                    )
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .min_w(px(0.))
                                                            .overflow_hidden()
                                                            .text_size(rems(1.0))
                                                            .text_color(name_color)
                                                            .child(entry.name),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .w(size_col_width)
                                                    .flex_none()
                                                    .text_size(rems(0.917))
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(if entry.is_dir {
                                                        "-".to_string()
                                                    } else {
                                                        format_bytes(entry.size)
                                                    }),
                                            )
                                            .child(
                                                div()
                                                    .w(modified_col_width)
                                                    .flex_none()
                                                    .text_size(rems(0.917))
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(format_mtime(entry.modified)),
                                            )
                                            .child(div().w(px(12.)).flex_none())
                                            .into_any_element()
                                    })
                                    .collect::<Vec<_>>()
                            }
                        })
                        .size_full()
                        .track_scroll(&self.remote_files_scroll_handle),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w(px(16.))
                            .child(
                                Scrollbar::vertical(&self.remote_files_scroll_handle)
                                    .scrollbar_show(ScrollbarShow::Always),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .h(px(24.))
                    .px_3()
                    .items_center()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().tab_bar)
                    .child(
                        div()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .text_size(rems(0.917))
                            .text_color(cx.theme().muted_foreground)
                            .child(status),
                    ),
            )
            .into_any_element()
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let sessions = self.config.sessions().to_vec();
        let active_session_id = self.active_session_id().map(ToOwned::to_owned);
        let connection_color = self.active_connection_color(cx);

        v_flex()
            .gap_4()
            .w_full()
            .h_full()
            .min_w(px(0.))
            .p_4()
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .bg(cx.theme().sidebar)
            .overflow_hidden()
            .child(
                h_flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .w(px(10.))
                            .h(px(10.))
                            .rounded_full()
                            .bg(connection_color),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .font_weight(FontWeight::BOLD)
                                    .text_size(rems(1.667))
                                    .text_color(cx.theme().primary)
                                    .child("ashell"),
                            )
                            .child(
                                div()
                                    .text_size(rems(0.917))
                                    .text_color(cx.theme().muted_foreground)
                                    .child({
                                        if let Some(kind) = self.active_kind() {
                                            let kind_str = match kind {
                                                TabKind::Local => t!("local_terminal").to_string(),
                                                TabKind::Ssh => "ssh".to_string(),
                                            };
                                            format!("{} / {}", kind_str, self.active_title())
                                        } else {
                                            self.active_title()
                                        }
                                    }),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .items_center()
                            .child(
                                div()
                                    .text_size(rems(1.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().primary)
                                    .child(t!("system")),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(rems(0.917))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(self.system_target_label()),
                            ),
                    )
                    .when_some(self.system_status.clone(), |this, text| {
                        this.child(
                            div()
                                .text_size(rems(0.917))
                                .text_color(cx.theme().muted_foreground)
                                .child(text),
                        )
                    })
                    .child(self.system_card(
                        t!("cpu").to_string(),
                        self.system.cpu_percent,
                        t!("global_load").into(),
                        cx.theme().chart_1,
                        cx,
                    ))
                    .child(self.system_card(
                        t!("mem").to_string(),
                        self.system.mem_percent,
                        self.system.mem_detail.clone(),
                        cx.theme().chart_2,
                        cx,
                    ))
                    .child(self.system_card(
                        t!("swap").to_string(),
                        self.system.swap_percent,
                        self.system.swap_detail.clone(),
                        cx.theme().chart_3,
                        cx,
                    ))
                    .child(self.network_card(cx))
                    .child(
                        v_flex()
                            .gap_2()
                            .p_2()
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().muted)
                            .child(
                                div()
                                    .text_size(rems(0.917))
                                    .text_color(cx.theme().chart_5)
                                    .child(t!("disk")),
                            )
                            .children({
                                let mut disk_elements = Vec::new();
                                for disk in self.system.disks.clone() {
                                    disk_elements.push(self.disk_row(disk, cx));
                                }
                                disk_elements
                            }),
                    ),
            )
            .child(
                Button::new("open-ssh-panel")
                    .primary()
                    .label(t!("add_ssh").to_string())
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_new_ssh_dialog(window, cx)),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h(px(0.))
                    .gap_2()
                    .child(
                        div()
                            .text_size(rems(1.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().primary)
                            .child(t!("saved")),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h(px(0.))
                            .size_full()
                            .child(
                                v_flex()
                                    .size_full()
                                    .id("saved-sessions-scroll")
                                    .track_scroll(&self.saved_scroll_handle)
                                    .overflow_y_scroll()
                                    .gap_2()
                                    .children(sessions.into_iter().enumerate().map(|(ix, session)| {
                                        let connect_id = session.id.clone();
                                        let edit_id = session.id.clone();
                                        let delete_id = session.id.clone();
                                        let is_active =
                                            active_session_id.as_deref() == Some(session.id.as_str());
                                        let name = session.name.clone();
                                        let detail = self.session_detail(&session);
                                        div()
                                            .id(("saved-connect", ix))
                                            .w_full()
                                            .p_2()
                                            .rounded_md()
                                            .border_1()
                                            .border_color(if is_active {
                                                cx.theme().primary
                                            } else {
                                                cx.theme().border
                                            })
                                            .bg(if is_active {
                                                cx.theme().tab_active
                                            } else {
                                                cx.theme().muted
                                            })
                                            .cursor_pointer()
                                            .hover(|this| this.bg(cx.theme().secondary))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, _, cx| {
                                                    this.connect_saved_session(connect_id.clone(), cx)
                                                }),
                                            )
                                            .context_menu({
                                                let view = cx.entity();
                                                move |menu, window, _| {
                                                    let edit_value = edit_id.clone();
                                                    let delete_value = delete_id.clone();
                                                    menu.item(PopupMenuItem::new("Edit").on_click(
                                                        window.listener_for(
                                                            &view,
                                                            move |this, _, window, cx| {
                                                                this.edit_saved_session(
                                                                    edit_value.clone(),
                                                                    window,
                                                                    cx,
                                                                )
                                                            },
                                                        ),
                                                    ))
                                                    .item(
                                                        PopupMenuItem::new("Delete").on_click(
                                                            window.listener_for(
                                                                &view,
                                                                move |this, _, _, cx| {
                                                                    this.remove_saved_session(
                                                                        delete_value.clone(),
                                                                        cx,
                                                                    )
                                                                },
                                                            ),
                                                        ),
                                                    )
                                                }
                                            })
                                            .child(
                                                v_flex()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_size(rems(1.0))
                                                            .font_weight(FontWeight::SEMIBOLD)
                                                            .child(name),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_size(rems(0.917))
                                                            .text_color(cx.theme().muted_foreground)
                                                            .child(detail),
                                                    ),
                                            )
                                    })),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .bottom_0()
                                    .left_0()
                                    .right_0()
                                    .child(
                                        gpui_component::scroll::Scrollbar::new(&self.saved_scroll_handle)
                                            .id("saved-scrollbar")
                                            .axis(gpui_component::scroll::ScrollbarAxis::Vertical)
                                            .scrollbar_show(gpui_component::scroll::ScrollbarShow::Always)
                                    )
                            )
                    ),
            )
    }
}

impl Render for Ashell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self
            .active_tab
            .as_ref()
            .is_some_and(|active_id| !self.tabs.iter().any(|tab| &tab.id == active_id))
        {
            self.active_tab = self.tabs.first().map(|tab| tab.id.clone());
            self.remote_sample_in_flight = false;
            self.request_active_system_snapshot();
        }
        self.sync_sftp_path_input(window, cx);
        self.sync_terminal_size(window, cx);
        if self.show_transfers_dialog {
            self.show_transfers_dialog = false;
            self.show_transfers_dialog(window, cx);
        }
        if let Some(new_display_offset) = self.terminal_scrollbar.future_display_offset.take() {
            if let Some(active_id) = self.active_tab.clone() {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == active_id) {
                    let current = tab.render_snapshot().display_offset;
                    match new_display_offset.cmp(&current) {
                        std::cmp::Ordering::Greater => {
                            tab.scroll_up_by(new_display_offset - current)
                        }
                        std::cmp::Ordering::Less => {
                            tab.scroll_down_by(current - new_display_offset)
                        }
                        std::cmp::Ordering::Equal => {}
                    }
                }
            }
        }
        let connection_color = self.active_connection_color(cx);
        let terminal_snapshot = self.active_snapshot();
        let active_tab_index = self
            .active_tab
            .as_ref()
            .and_then(|active_id| self.tabs.iter().position(|tab| &tab.id == active_id));
        if let Some(snapshot) = terminal_snapshot.as_ref() {
            self.terminal_scrollbar
                .update(snapshot, px(self.terminal_line_height()));
        }

        div()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_family(self.ui_font_family.clone())
            .child(
                h_resizable("ashell-workspace")
                    .with_state(&self.workspace_panels)
                    .child(
                        resizable_panel()
                            .size(px(self.config.workspace_panels().and_then(|s| s.first().copied()).unwrap_or(SIDEBAR_WIDTH)))
                            .size_range(px(240.)..px(520.))
                            .flex_none()
                            .child(self.sidebar(cx)),
                    )
                    .child(
                        resizable_panel().child(
                            v_flex()
                                .size_full()
                                .relative()
                                .overflow_hidden()
                                .child(
                                    v_flex()
                                        .h(px(40.))
                                        .w_full()
                                        .flex_none()
                                        .bg(cx.theme().tab_bar)
                                        .child(
                                            h_flex()
                                                .h_full()
                                                .w_full()
                                                .items_center()
                                                .gap_2()
                                                .px_3()
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w(px(0.))
                                                        .h_full()
                                                        .overflow_x_hidden()
                                                        .child(
                                                            TabBar::new("ashell-tab-bar")
                                                                .track_scroll(&self.tabs_scroll_handle)
                                                                .selected_index(active_tab_index.unwrap_or(0))
                                                                .children(self.tabs.iter().enumerate().map(|(ix, tab)| {
                                                                    let id = tab.id.clone();
                                                                    let close_id = tab.id.clone();
                                                                    Tab::new()
                                                                        .label(tab.title.clone())
                                                                        .on_click(cx.listener(move |this, _, window, cx| {
                                                                            this.activate_tab(id.clone(), window, cx)
                                                                        }))
                                                                        .suffix(
                                                                            Button::new(("tab-close", ix))
                                                                                .ghost()
                                                                                .xsmall()
                                                                                .icon(IconName::Close)
                                                                                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                                                                                    window.prevent_default();
                                                                                    cx.stop_propagation();
                                                                                })
                                                                                .on_click(cx.listener(move |this, _, window, cx| {
                                                                                    window.prevent_default();
                                                                                    cx.stop_propagation();
                                                                                    this.close_tab(close_id.clone(), cx)
                                                                                })),
                                                                        )
                                                                }))
                                                                .last_empty_space(div().w_3())
                                                                .w_full()
                                                                .h_full(),
                                                        ),
                                                )
                                                .child(
                                                    h_flex()
                                                        .flex_none()
                                                        .items_center()
                                                        .gap_2()
                                                        .child(
                                                            Button::new("open-selector")
                                                                .ghost()
                                                                .small()
                                                                .icon(IconName::Plus)
                                                                .on_click(cx.listener(|this, _, window, cx| this.show_selector_dialog(window, cx))),
                                                        )
                                                        .child(
                                                            Button::new("open-settings")
                                                                .ghost()
                                                                .small()
                                                                .icon(IconName::Settings2)
                                                                .on_click(cx.listener(|this, _, window, cx| this.show_settings_dialog(window, cx))),
                                                        )
                                                        .child(self.theme_dropdown(cx))
                                                        .child(
                                                            div()
                                                                .w(px(10.))
                                                                .h(px(10.))
                                                                .rounded_full()
                                                                .bg(connection_color),
                                                        ),
                                                ),
                                        ),
                                )
                                .child(
                                    v_resizable("ashell-main-panels")
                                        .with_state(&self.body_panels)
                                        .child(
                                            resizable_panel().child(
                                                div()
                                            .size_full()
                                            .p_4()
                                            .bg(cx.theme().background)
                                            .text_color(cx.theme().foreground)
                                            .font_family(self.ui_font_family.clone())
                                            .text_size(rems(1.083))
                                            .line_height(px(18.))
                                            .overflow_hidden()
                                            .track_focus(&self.focus_handle)
                                            .key_context(TERMINAL_KEY_CONTEXT)
                                            .on_mouse_down(MouseButton::Left, cx.listener(Self::focus_terminal))
                                            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_terminal_right_click))
                                            .on_mouse_move(cx.listener(Self::on_terminal_mouse_move))
                                            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_terminal_mouse_up))
                                            .on_key_down(cx.listener(Self::on_terminal_key_down))
                                            .on_action(cx.listener(Self::on_terminal_tab_action))
                                            .on_action(cx.listener(Self::on_terminal_backtab_action))
                                            .on_scroll_wheel(cx.listener(Self::on_terminal_scroll))
                                            .child(match terminal_snapshot.clone() {
                                                None => self.render_home_page(cx).into_any_element(),
                                                Some(snapshot) => h_flex()
                                                    .w_full()
                                                    .h_full()
                                                    .gap_2()
                                                    .child(
                                                        div()
                                                            .size_full()
                                                            .on_prepaint({
                                                                let view = cx.entity().clone();
                                                                move |bounds, _window, cx| {
                                                                    let _ = view.update(cx, |this, _| {
                                                                        this.terminal_bounds = Some(bounds);
                                                                    });
                                                                }
                                                            })
                                                            .child(
                                                                TerminalElement::new(
                                                                    cx.entity(),
                                                                    self.focus_handle.clone(),
                                                                    snapshot,
                                                                    self.terminal_marked_text.clone(),
                                                                    self.terminal_font_family.clone(),
                                                                    px(self.terminal_font_size),
                                                                    px(self.terminal_line_height()),
                                                                    px(self.terminal_cell_width()),
                                                                ),
                                                            )
                                                            .vertical_scrollbar(&self.terminal_scrollbar)
                                                            .into_any_element(),
                                                    )
                                                    .into_any_element(),
                                            }),
                                    ),
                                        )
                                        .child(
                                            resizable_panel()
                                                .size(px(self.config.body_panels().and_then(|s| s.get(1).copied()).unwrap_or(248.0)))
                                                .size_range(px(180.)..px(1000.))
                                                .child(self.render_sftp_panel(window, cx)),
                                        ),
                                ),
                        ),
                    ),
            )
            .children(Root::render_dialog_layer(window, cx))

            .children(Root::render_sheet_layer(window, cx))
            .when_some(self.sftp_context_menu.clone(), |this, menu| {
                let label = if menu.is_dir {
                    t!("download_folder").to_string()
                } else {
                    t!("download").to_string()
                };
                this.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                            this.dismiss_sftp_context_menu(cx);
                        }))
                        .on_mouse_down(MouseButton::Right, cx.listener(|this, _, _, cx| {
                            this.dismiss_sftp_context_menu(cx);
                        }))
                        .child(
                            div()
                                .absolute()
                                .left(menu.position.x)
                                .top(menu.position.y)
                                .w(px(172.))
                                .p_1()
                                .rounded_md()
                                .border_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().popover)
                                .shadow_lg()
                                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                                    window.prevent_default();
                                    cx.stop_propagation();
                                })
                                .on_mouse_down(MouseButton::Right, |_, window, cx| {
                                    window.prevent_default();
                                    cx.stop_propagation();
                                })
                                .child(
                                    v_flex()
                                        .w_full()
                                        .child(
                                            Button::new("sftp-context-download")
                                                .ghost()
                                                .w_full()
                                                .justify_start()
                                                .label(label)
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.trigger_sftp_context_download(window, cx);
                                                })),
                                        )
                                        .when(!menu.is_dir && is_editable_text_file(&menu.remote_path), |this| {
                                            this.child(
                                                Button::new("sftp-context-edit")
                                                    .ghost()
                                                    .w_full()
                                                    .justify_start()
                                                    .label(t!("edit_file"))
                                                    .tooltip(t!("edit_file_tooltip").to_string())
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.trigger_sftp_context_edit(cx);
                                                    }))
                                            )
                                        }),
                                ),
                        ),
                )
            })
            .when_some(self.connection_progress.clone(), |this, progress| {
                this.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .bg(Hsla {
                            h: 0.0,
                            s: 0.0,
                            l: 0.0,
                            a: 0.48,
                        })
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .w(px(420.))
                                .p_5()
                                .rounded_lg()
                                .border_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().popover)
                                .shadow_lg()
                                .child(
                                    v_flex()
                                        .gap_4()
                                        .child(
                                            Button::new("ssh-connect-progress")
                                                .primary()
                                                .loading(!progress.failed)
                                                .label(progress.title.clone()),
                                        )
                                        .child(
                                            div()
                                                .max_h(px(220.))
                                                .overflow_y_scrollbar()
                                                .child(
                                                    v_flex()
                                                        .gap_2()
                                                        .children(progress.lines.iter().cloned().map(|line| {
                                                            div()
                                                                .text_size(rems(1.0))
                                                                .text_color(if progress.failed {
                                                                    cx.theme().danger
                                                                } else {
                                                                    cx.theme().muted_foreground
                                                                })
                                                                .child(line)
                                                        })),
                                                ),
                                        )
                                        .when(progress.failed, |this| {
                                            this.child(
                                                h_flex()
                                                    .justify_end()
                                                    .gap_2()
                                                    .child(
                                                        Button::new("ssh-connect-progress-retry")
                                                            .primary()
                                                            .label(t!("retry").to_string())
                                                            .on_click(cx.listener(|this, _, _, cx| {
                                                                this.retry_connection_progress(cx)
                                                            })),
                                                    )
                                                    .child(
                                                        Button::new("ssh-connect-progress-close")
                                                            .label(t!("cancel").to_string())
                                                            .on_click(cx.listener(|this, _, _, cx| {
                                                                this.cancel_connection_progress(cx)
                                                            })),
                                                    ),
                                            )
                                        }),
                                ),
                        ),
                )
            })
    }
}
fn load_fonts(cx: &mut App) -> Result<()> {
    let regular =
        Cow::Borrowed(include_bytes!("../assets/fonts/MapleMono-NF-CN-Regular.ttf").as_slice());
    let bold = Cow::Borrowed(include_bytes!("../assets/fonts/MapleMono-NF-CN-Bold.ttf").as_slice());
    cx.text_system()
        .add_fonts(vec![regular, bold])
        .context("load Maple Mono NF CN fonts")?;
    // At startup we don't have a config yet, so pass the default UI font family.
    // It will be reapplied in Ashell::new() -> apply_theme_preferences.
    set_theme_font_names(cx.global_mut::<Theme>(), ".SystemUIFont");
    Ok(())
}

fn load_embedded_themes(cx: &mut App) {
    let registry = ThemeRegistry::global_mut(cx);
    for theme_json in EMBEDDED_THEME_JSONS {
        if let Err(err) = registry.load_themes_from_str(theme_json) {
            tracing::warn!("failed to load embedded theme: {err:#}");
        }
    }
}

fn set_theme_font_names(theme: &mut Theme, ui_font_family: &str) {
    theme.font_family = ui_font_family.into();
    theme.mono_font_family = ui_font_family.into();
}

#[cfg(target_os = "macos")]
fn sync_macos_launch_environment() {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let Ok(output) = Command::new(&shell).args(["-l", "-c", "env -0"]).output() else {
        return;
    };
    if !output.status.success() {
        return;
    }

    for entry in output.stdout.split(|b| *b == 0) {
        if entry.is_empty() {
            continue;
        }
        let Some(eq) = entry.iter().position(|b| *b == b'=') else {
            continue;
        };
        let Ok(key) = std::str::from_utf8(&entry[..eq]) else {
            continue;
        };
        let Ok(value) = std::str::from_utf8(&entry[eq + 1..]) else {
            continue;
        };

        let should_import = matches!(
            key,
            "PATH"
                | "MANPATH"
                | "INFOPATH"
                | "LANG"
                | "LC_ALL"
                | "LC_CTYPE"
                | "SHELL"
                | "HOME"
                | "HOMEBREW_PREFIX"
                | "HOMEBREW_CELLAR"
                | "HOMEBREW_REPOSITORY"
        ) || key.starts_with("LC_");

        if should_import {
            unsafe {
                std::env::set_var(key, value);
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn sync_macos_launch_environment() {}

fn open_main_window(cx: &mut App) {
    let mut window_options = WindowOptions::default();

    #[cfg(not(target_os = "macos"))]
    if let Ok(img) = image::load_from_memory(include_bytes!("../assets/icons/ashell.png")) {
        window_options.icon = Some(std::sync::Arc::new(img.into_rgba8()));
    }

    let config = ConfigStore::load().unwrap_or_else(|_| ConfigStore::in_memory());
    if let Some(bounds) = config.window_bounds() {
        window_options.window_bounds = Some(match bounds {
            crate::config::SavedWindowBounds::Fullscreen {
                x,
                y,
                width,
                height,
            } => gpui::WindowBounds::Fullscreen(Bounds::new(
                point(px(*x), px(*y)),
                size(px(*width), px(*height)),
            )),
            crate::config::SavedWindowBounds::Maximized {
                x,
                y,
                width,
                height,
            } => gpui::WindowBounds::Maximized(Bounds::new(
                point(px(*x), px(*y)),
                size(px(*width), px(*height)),
            )),
            crate::config::SavedWindowBounds::Windowed {
                x,
                y,
                width,
                height,
            } => gpui::WindowBounds::Windowed(Bounds::new(
                point(px(*x), px(*y)),
                size(px(*width), px(*height)),
            )),
        });
    } else if let Some(display) = cx.displays().first().cloned() {
        let display_bounds = display.bounds();
        let width = display_bounds.size.width * 0.8;
        let height = display_bounds.size.height * 0.9;

        let x = display_bounds.origin.x + (display_bounds.size.width - width) / 2.0;

        #[cfg(target_os = "macos")]
        let y = display_bounds.origin.y;
        #[cfg(not(target_os = "macos"))]
        let y = display_bounds.origin.y + (display_bounds.size.height - height) / 2.0;

        window_options.window_bounds = Some(gpui::WindowBounds::Windowed(Bounds::new(
            point(x, y),
            size(width, height),
        )));
    }

    cx.open_window(window_options, |window, cx| {
        window.activate_window();
        window.set_window_title("ashell");
        Theme::sync_system_appearance(Some(window), cx);
        let view = cx.new(|cx| Ashell::new(window, cx));

        let workspace_panels_clone = view.read(cx).workspace_panels.clone();
        let body_panels_clone = view.read(cx).body_panels.clone();
        window.on_window_should_close(cx, move |window: &mut gpui::Window, cx: &mut gpui::App| {
            let mut config = ConfigStore::load().unwrap_or_else(|_| ConfigStore::in_memory());
            let current_bounds = window.window_bounds();
            let saved_bounds = match current_bounds {
                gpui::WindowBounds::Fullscreen(b) => crate::config::SavedWindowBounds::Fullscreen {
                    x: b.origin.x.into(),
                    y: b.origin.y.into(),
                    width: b.size.width.into(),
                    height: b.size.height.into(),
                },
                gpui::WindowBounds::Maximized(b) => crate::config::SavedWindowBounds::Maximized {
                    x: b.origin.x.into(),
                    y: b.origin.y.into(),
                    width: b.size.width.into(),
                    height: b.size.height.into(),
                },
                gpui::WindowBounds::Windowed(b) => crate::config::SavedWindowBounds::Windowed {
                    x: b.origin.x.into(),
                    y: b.origin.y.into(),
                    width: b.size.width.into(),
                    height: b.size.height.into(),
                },
            };
            let workspace_sizes: Vec<f32> = workspace_panels_clone
                .read(cx)
                .sizes()
                .iter()
                .map(|s| s.into())
                .collect();
            let body_sizes: Vec<f32> = body_panels_clone
                .read(cx)
                .sizes()
                .iter()
                .map(|s| s.into())
                .collect();
            config.set_layout_state(Some(saved_bounds), Some(workspace_sizes), Some(body_sizes));
            let _ = config.save();
            true
        });

        cx.new(|cx| Root::new(view, window, cx))
    })
    .expect("failed to open window");
}

fn main() {
    sync_macos_launch_environment();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    #[cfg(target_os = "macos")]
    let app = gpui_platform::application()
        .with_assets(Assets)
        .with_quit_mode(QuitMode::Explicit);

    #[cfg(not(target_os = "macos"))]
    let app = gpui_platform::application().with_assets(Assets);
    app.on_reopen(|cx| {
        if cx.windows().is_empty() {
            open_main_window(cx);
        }
    });
    app.run(move |cx| {
        gpui_component::init(cx);
        cx.bind_keys([
            KeyBinding::new("tab", TerminalTabKey, Some(TERMINAL_KEY_CONTEXT)),
            KeyBinding::new("shift-tab", TerminalBacktabKey, Some(TERMINAL_KEY_CONTEXT)),
        ]);
        load_embedded_themes(cx);
        if let Err(err) = load_fonts(cx) {
            tracing::warn!("failed to load embedded fonts: {err:#}");
        }
        open_main_window(cx);
    });
}

fn is_editable_text_file(filename: &str) -> bool {
    let lower = filename.to_lowercase();
    let ext = std::path::Path::new(&lower).extension().and_then(|s| s.to_str()).unwrap_or("");
    let known_exts = ["txt", "conf", "json", "yaml", "yml", "xml", "ini", "sh", "py", "rs", "js", "ts", "html", "css", "md", "toml", "csv", "log", "cfg"];
    if known_exts.contains(&ext) {
        return true;
    }
    let known_names = ["dockerfile", "makefile", ".gitignore", ".env"];
    if known_names.contains(&lower.as_str()) {
        return true;
    }
    false
}

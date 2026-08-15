pub mod config;
pub mod ssh_config;
pub mod ssh_keys;

use base64::Engine as _;
use gpui::{
    AppContext as _, Context, Entity, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    SharedString, Window, px,
};
use gpui_component::{Theme, WindowExt as _, input::InputState};
use rust_i18n::t;
use uuid::Uuid;

use self::config::{
    AuthMethod, SavedPaneLayout, SavedTabGroup, SavedTabsState, SavedTerminalTab, Session,
};

use crate::{
    Ashell, PaneLayout, SelectorEntry, TabGroup,
    app::constants::{DEFAULT_COLS, DEFAULT_ROWS},
    backend::{local, ssh},
    terminal::{BackendCommand, RenderSnapshot, TabKind, TerminalTab},
    text_encoding::TextEncoding,
};

pub(crate) fn compact_local_path(path: &std::path::Path) -> String {
    if let Some(base_dirs) = directories::BaseDirs::new() {
        if let Some(relative_path) = relative_to_home(path, base_dirs.home_dir()) {
            if relative_path.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~{}{}", std::path::MAIN_SEPARATOR, relative_path.display());
        }
    }

    path.display().to_string()
}

fn relative_to_home(path: &std::path::Path, home: &std::path::Path) -> Option<std::path::PathBuf> {
    if let Ok(relative) = path.strip_prefix(home) {
        return Some(relative.to_path_buf());
    }

    #[cfg(windows)]
    {
        let mut path_components = path.components();
        for home_component in home.components() {
            let path_component = path_components.next()?;
            if !path_component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&home_component.as_os_str().to_string_lossy())
            {
                return None;
            }
        }
        return Some(path_components.as_path().to_path_buf());
    }

    #[cfg(not(windows))]
    None
}

pub(crate) fn decode_local_path_title(encoded: &str) -> Option<std::path::PathBuf> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    let path = String::from_utf8(bytes).ok()?;
    let path = std::path::PathBuf::from(path);
    path.is_absolute().then_some(path)
}

fn default_local_directory() -> Option<std::path::PathBuf> {
    directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .filter(|path| path.is_dir())
}

fn initial_local_title() -> String {
    default_local_directory()
        .map(|path| compact_local_path(&path))
        .unwrap_or_else(|| {
            if cfg!(windows) {
                "PowerShell".to_string()
            } else {
                "Local".to_string()
            }
        })
}

fn connecting_sftp_state() -> crate::terminal::SftpUiState {
    let mut expanded_directories = std::collections::HashSet::new();
    expanded_directories.insert("/".to_string());
    crate::terminal::SftpUiState {
        current_path: "/".into(),
        status: rust_i18n::t!("sftp_connecting").to_string(),
        directory_cache: std::collections::HashMap::new(),
        expanded_directories,
        loading_directories: std::collections::HashSet::new(),
        directory_errors: std::collections::HashMap::new(),
        selected_path: None,
        preview: None,
        selected_entries: std::collections::HashSet::new(),
        home_dir: "/".into(),
        home_dir_resolved: false,
    }
}

fn save_pane_layout(layout: &PaneLayout) -> SavedPaneLayout {
    match layout {
        PaneLayout::Single(tab_id) => SavedPaneLayout::Single {
            tab_id: tab_id.clone(),
        },
        PaneLayout::Horizontal(children, ratio) => SavedPaneLayout::Horizontal {
            children: children.iter().map(save_pane_layout).collect(),
            ratio: *ratio,
        },
        PaneLayout::Vertical(children, ratio) => SavedPaneLayout::Vertical {
            children: children.iter().map(save_pane_layout).collect(),
            ratio: *ratio,
        },
    }
}

fn restore_pane_layout(layout: &SavedPaneLayout) -> PaneLayout {
    match layout {
        SavedPaneLayout::Single { tab_id } => PaneLayout::Single(tab_id.clone()),
        SavedPaneLayout::Horizontal { children, ratio } => PaneLayout::Horizontal(
            children.iter().map(restore_pane_layout).collect(),
            (*ratio).clamp(0.1, 0.9),
        ),
        SavedPaneLayout::Vertical { children, ratio } => PaneLayout::Vertical(
            children.iter().map(restore_pane_layout).collect(),
            (*ratio).clamp(0.1, 0.9),
        ),
    }
}

impl Ashell {
    pub(crate) fn apply_local_directory_change(&mut self, tab_id: &str, path: std::path::PathBuf) {
        let title = compact_local_path(&path);
        let is_local = if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            if tab.kind == TabKind::Local {
                if tab.local_cwd.as_ref() == Some(&path) && tab.title == title {
                    return;
                }
                tab.title = title.clone();
                tab.dynamic_title = title.clone();
                tab.local_cwd = Some(path);
                true
            } else {
                false
            }
        } else {
            false
        };

        if !is_local {
            return;
        }

        if let Some(group) = self
            .tab_groups
            .iter_mut()
            .find(|group| group.pane_root.contains(tab_id))
        {
            let is_focused = self.active_tab.as_deref() == Some(tab_id);
            let is_single_pane = matches!(&group.pane_root, PaneLayout::Single(_));
            if is_focused || is_single_pane {
                group.title = title;
            }
        }
        self.save_tabs_state_background();
    }

    pub(crate) fn capture_tabs_state(&mut self) {
        if !self.config.remember_tabs() {
            self.config.set_saved_tabs(None);
            return;
        }

        self.sync_pane_root_to_group();
        let default_local_cwd = default_local_directory();
        let groups = self
            .tab_groups
            .iter()
            .filter_map(|group| {
                let pane_ids = group.pane_root.tab_ids();
                let tabs = pane_ids
                    .iter()
                    .filter_map(|tab_id| {
                        let tab = self.tabs.iter().find(|tab| tab.id.as_str() == *tab_id)?;
                        match tab.kind {
                            TabKind::Local => Some(SavedTerminalTab::Local {
                                id: tab.id.clone(),
                                cwd: tab.local_cwd.clone().or_else(|| default_local_cwd.clone()),
                                terminal_encoding: tab.text_encoding(),
                            }),
                            TabKind::Ssh => {
                                tab.session.clone().map(|session| SavedTerminalTab::Ssh {
                                    id: tab.id.clone(),
                                    session,
                                })
                            }
                            TabKind::Serial => {
                                tab.session.clone().map(|session| SavedTerminalTab::Serial {
                                    id: tab.id.clone(),
                                    session,
                                })
                            }
                        }
                    })
                    .collect::<Vec<_>>();

                if tabs.len() != pane_ids.len() {
                    tracing::warn!(
                        "[session] skipped tab group '{}' because its panes could not be saved",
                        group.id
                    );
                    return None;
                }

                Some(SavedTabGroup {
                    id: group.id.clone(),
                    title: group.title.clone(),
                    pane_root: save_pane_layout(&group.pane_root),
                    tabs,
                })
            })
            .collect();

        self.config.set_saved_tabs(Some(SavedTabsState {
            groups,
            active_group: self.active_group.clone(),
            active_tab: self.active_tab.clone(),
        }));
    }

    pub(crate) fn save_tabs_state_background(&mut self) {
        if self.config.remember_tabs() {
            self.capture_tabs_state();
            self.save_preferences_background();
        }
    }

    pub(crate) fn restore_saved_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.config.remember_tabs() {
            return;
        }
        let Some(saved_state) = self.config.saved_tabs().cloned() else {
            return;
        };

        let requested_active_group = saved_state.active_group;
        let requested_active_tab = saved_state.active_tab;
        let mut restored_group_ids = std::collections::HashSet::new();
        let mut restored_tab_ids = std::collections::HashSet::new();

        for saved_group in saved_state.groups {
            let SavedTabGroup {
                id: group_id,
                title,
                pane_root,
                tabs,
            } = saved_group;
            if group_id.is_empty() || !restored_group_ids.insert(group_id.clone()) {
                continue;
            }

            let mut group_tab_ids = std::collections::HashSet::new();
            for saved_tab in tabs {
                let (tab_id, mut tab) = match saved_tab {
                    SavedTerminalTab::Local {
                        id,
                        cwd,
                        terminal_encoding,
                    } => {
                        if id.is_empty() || restored_tab_ids.contains(&id) {
                            continue;
                        }
                        let cwd = cwd
                            .filter(|path| path.is_absolute() && path.is_dir())
                            .or_else(default_local_directory);
                        let title = cwd
                            .as_deref()
                            .map(compact_local_path)
                            .unwrap_or_else(initial_local_title);
                        let backend_events =
                            crate::terminal::GuardedBackendEventSender::new(self.events_tx.clone());
                        let backend = match local::spawn_local_terminal_at(
                            id.clone(),
                            DEFAULT_COLS,
                            DEFAULT_ROWS,
                            backend_events.clone(),
                            cwd.as_deref(),
                        ) {
                            Ok(backend) => backend,
                            Err(err) => {
                                tracing::warn!(
                                    "[session] failed to restore local tab '{}': {err:#}",
                                    id
                                );
                                continue;
                            }
                        };
                        let mut tab =
                            TerminalTab::new_local(id.clone(), title, backend, backend_events);
                        tab.local_cwd = cwd;
                        tab.set_text_encoding(terminal_encoding);
                        (id, tab)
                    }
                    SavedTerminalTab::Ssh { id, session } => {
                        if id.is_empty() || restored_tab_ids.contains(&id) {
                            continue;
                        }
                        let backend_events =
                            crate::terminal::GuardedBackendEventSender::new(self.events_tx.clone());
                        let mut tab = TerminalTab::new_ssh(
                            id.clone(),
                            &session,
                            crate::terminal::BackendTx::Pending,
                            backend_events,
                        );
                        let pending_reason = t!("ssh_reconnect_pending").to_string();
                        tab.status = pending_reason.clone();
                        tab.disconnected_reason = Some(pending_reason);
                        (id, tab)
                    }
                    SavedTerminalTab::Serial { id, session } => {
                        if id.is_empty() || restored_tab_ids.contains(&id) {
                            continue;
                        }
                        let backend_events =
                            crate::terminal::GuardedBackendEventSender::new(self.events_tx.clone());
                        let backend = crate::backend::serial::spawn_serial_client(
                            self.runtime.handle(),
                            id.clone(),
                            session.clone(),
                            backend_events.clone(),
                        );
                        (
                            id.clone(),
                            TerminalTab::new_serial(
                                id,
                                &session,
                                crate::terminal::BackendTx::Serial(backend),
                                backend_events,
                            ),
                        )
                    }
                };
                tab.resize(DEFAULT_COLS, DEFAULT_ROWS);
                group_tab_ids.insert(tab_id.clone());
                restored_tab_ids.insert(tab_id);
                self.tabs.push(tab);
            }

            let mut pane_root = restore_pane_layout(&pane_root);
            let missing_tabs = pane_root
                .tab_ids()
                .into_iter()
                .filter(|tab_id| !group_tab_ids.contains(*tab_id))
                .map(str::to_string)
                .collect::<Vec<_>>();
            for tab_id in missing_tabs {
                pane_root.remove_tab(&tab_id);
            }
            let layout_tab_ids = pane_root
                .tab_ids()
                .into_iter()
                .filter(|tab_id| !tab_id.is_empty())
                .map(str::to_string)
                .collect::<std::collections::HashSet<_>>();
            let orphaned_tab_ids = group_tab_ids
                .difference(&layout_tab_ids)
                .cloned()
                .collect::<Vec<_>>();
            for tab_id in orphaned_tab_ids {
                if let Some(tab) = self.tabs.iter().find(|tab| tab.id == tab_id) {
                    tab.send_backend(BackendCommand::Close);
                }
                self.tabs.retain(|tab| tab.id != tab_id);
                restored_tab_ids.remove(&tab_id);
            }
            if pane_root
                .tab_ids()
                .first()
                .is_none_or(|tab_id| tab_id.is_empty())
            {
                continue;
            }

            self.tab_groups.push(TabGroup {
                id: group_id.clone(),
                title,
                pane_root,
                // Restored SSH sessions remain offline until the user confirms
                // reconnecting, so their SFTP worker must remain stopped too.
                sftp: None,
                sftp_tab_id: None,
            });
        }

        let active_group = requested_active_group
            .filter(|id| self.tab_groups.iter().any(|group| group.id == *id))
            .or_else(|| self.tab_groups.first().map(|group| group.id.clone()));
        let Some(active_group) = active_group else {
            return;
        };
        let Some(active_layout) = self
            .tab_groups
            .iter()
            .find(|group| group.id == active_group)
            .map(|group| group.pane_root.clone())
        else {
            return;
        };

        self.active_group = Some(active_group.clone());
        self.pane_root = active_layout;
        let active_tab = requested_active_tab
            .filter(|id| self.pane_root.contains(id))
            .or_else(|| self.pane_root.tab_ids().first().map(|id| (*id).to_string()));
        if let Some(active_tab) = active_tab {
            self.focus_pane_with_id(active_tab);
        }
        if let Some(group_index) = self
            .tab_groups
            .iter()
            .position(|group| group.id == active_group)
        {
            self.tabs_scroll_handle.scroll_to_item(group_index);
        }
        self.pending_sftp_path_sync = Some("/".into());
        self.sync_sftp_to_active_tab();
        self.sync_system_tab_to_active_group();
        self.status = "tabs restored".into();

        // Defer the dialog until the restored view has been mounted. This also
        // ensures that only the currently focused restored SSH tab is prompted.
        let prompt_tab_id = self.active_tab.as_ref().and_then(|tab_id| {
            self.tabs
                .iter()
                .find(|tab| tab.id == *tab_id && tab.kind == TabKind::Ssh && !tab.connected)
                .map(|tab| tab.id.clone())
        });
        if let Some(prompt_tab_id) = prompt_tab_id {
            let view = cx.entity();
            window.defer(cx, move |window, cx| {
                view.update(cx, |this, cx| {
                    this.show_ssh_reconnect_dialog(prompt_tab_id, window, cx);
                });
            });
        }
        cx.notify();
    }

    pub(crate) fn open_local(&mut self, cx: &mut Context<Self>) {
        let id = Uuid::new_v4().to_string();
        let initial_directory = default_local_directory();
        let backend_events =
            crate::terminal::GuardedBackendEventSender::new(self.events_tx.clone());
        match local::spawn_local_terminal_at(
            id.clone(),
            DEFAULT_COLS,
            DEFAULT_ROWS,
            backend_events.clone(),
            initial_directory.as_deref(),
        ) {
            Ok(backend) => {
                let title = initial_local_title();
                let mut tab =
                    TerminalTab::new_local(id.clone(), title.clone(), backend, backend_events);
                tab.local_cwd = initial_directory;
                tab.resize(DEFAULT_COLS, DEFAULT_ROWS);
                self.tabs.push(tab);
                self.active_tab = Some(id.clone());
                self.pane_root = PaneLayout::Single(id.clone());
                self.focused_pane_path = vec![];
                let group_id = Uuid::new_v4().to_string();
                self.tab_groups.push(TabGroup {
                    id: group_id.clone(),
                    title,
                    pane_root: PaneLayout::Single(id),
                    sftp: None,
                    sftp_tab_id: None,
                });
                self.active_group = Some(group_id);
                self.tabs_scroll_handle.scroll_to_item(self.tabs.len() - 1);
                self.sync_system_tab_to_active_group();
                self.status = "local terminal opened".into();
            }
            Err(err) => {
                self.status = format!("failed to open local terminal: {err:#}").into();
            }
        }
        self.save_tabs_state_background();
        cx.notify();
    }

    pub(crate) fn connect_ssh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session_protocol == "serial" {
            let session_name = self.session_name_input.read(cx).value().trim().to_string();
            let port_name = self.host_input.read(cx).value().trim().to_string();
            let baud_rate = self
                .baud_rate_input
                .read(cx)
                .value()
                .trim()
                .parse::<u32>()
                .unwrap_or(115200);

            if port_name.is_empty() {
                self.status = "Serial port path is required".into();
                cx.notify();
                return;
            }

            let name = if session_name.is_empty() {
                port_name.clone()
            } else {
                session_name
            };

            let is_editing = self.editing_session_id.is_some();
            let existing_id = self.editing_session_id.clone();
            let existing_last_used = existing_id
                .as_deref()
                .and_then(|id| self.config.get(id))
                .and_then(|session| session.last_used.clone());

            let mut session = Session::serial(port_name, baud_rate);
            session.name = name;
            if let Some(id) = existing_id {
                session.id = id;
            }
            session.last_used = existing_last_used;

            self.config.upsert(session.clone());
            if let Err(err) = self.config.save() {
                tracing::warn!("failed to save config: {err:#}");
            }

            if !is_editing {
                self.open_serial_session(session, cx);
            }
            self.editing_session_id = None;
            self.active_dialog = None;
            window.close_dialog(cx);
            cx.notify();
            return;
        }

        tracing::info!("[ui] user initiating new ssh connection from form");
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
        let passphrase = self.passphrase_input.read(cx).value().to_string();

        if host.is_empty() || user.is_empty() {
            self.status = t!("host_and_user_required").into();
            cx.notify();
            return;
        }

        if self.ssh_proxy_type != "none" {
            let proxy_host = self.proxy_host_input.read(cx).value().trim().to_string();
            let proxy_port_str = self.proxy_port_input.read(cx).value().trim().to_string();
            let proxy_port = proxy_port_str.parse::<u16>().ok();
            if proxy_host.is_empty() || proxy_port.is_none() {
                self.status = "Proxy host and port are required".into();
                cx.notify();
                return;
            }
        }

        let name = if session_name.is_empty() {
            host.clone()
        } else {
            session_name
        };
        let is_editing = self.editing_session_id.is_some();
        let existing_id = self.editing_session_id.clone();
        let existing_last_used = existing_id
            .as_deref()
            .and_then(|id| self.config.get(id))
            .and_then(|session| session.last_used.clone());

        let mut session = match self.ssh_auth_method {
            AuthMethod::Password => Session::password(host, port, user, password),
            AuthMethod::Key => Session::key(host, port, user, key_path, key_inline, passphrase),
            AuthMethod::Config => {
                // Force key_inline to empty — config mode never uses inline key content.
                // The backend will try default keys from ~/.ssh/ if no explicit key path is set.
                let mut session =
                    Session::key(host, port, user, key_path, String::new(), String::new());
                session.auth = AuthMethod::Config;
                session
            }
        };
        session.name = name;
        if let Some(id) = existing_id {
            session.id = id;
        }
        session.last_used = existing_last_used;
        session.proxy_type = self.ssh_proxy_type.clone();
        session.proxy_host = self.proxy_host_input.read(cx).value().trim().to_string();
        session.proxy_port = self
            .proxy_port_input
            .read(cx)
            .value()
            .trim()
            .parse::<u16>()
            .ok();
        session.proxy_user = self.proxy_user_input.read(cx).value().trim().to_string();
        session.proxy_password = self.proxy_password_input.read(cx).value().to_string();
        session.terminal_encoding = self.ssh_terminal_encoding;
        self.config.upsert(session.clone());
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save config: {err:#}");
        }

        if !is_editing {
            self.open_ssh_session(session, cx);
        }
        self.editing_session_id = None;
        self.active_dialog = None;
        window.close_dialog(cx);
        cx.notify();
    }

    pub(crate) fn set_input_value(
        input: &Entity<InputState>,
        value: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        input.update(cx, |state, cx| state.set_value(value, window, cx));
    }

    pub(crate) fn reset_ssh_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing_session_id = None;
        self.ssh_auth_method = AuthMethod::Password;
        self.ssh_config_selected = None;
        self.session_protocol = "ssh".to_string();
        self.ssh_terminal_encoding = TextEncoding::Utf8;
        Self::set_input_value(&self.session_name_input, "", window, cx);
        Self::set_input_value(&self.host_input, "", window, cx);
        Self::set_input_value(&self.port_input, "22", window, cx);
        Self::set_input_value(&self.user_input, "root", window, cx);
        Self::set_input_value(&self.password_input, "", window, cx);
        Self::set_input_value(&self.key_path_input, "", window, cx);
        Self::set_input_value(&self.key_inline_input, "", window, cx);
        Self::set_input_value(&self.passphrase_input, "", window, cx);
        Self::set_input_value(&self.baud_rate_input, "115200", window, cx);
        self.ssh_proxy_type = "none".to_string();
        Self::set_input_value(&self.proxy_host_input, "", window, cx);
        Self::set_input_value(&self.proxy_port_input, "", window, cx);
        Self::set_input_value(&self.proxy_user_input, "", window, cx);
        Self::set_input_value(&self.proxy_password_input, "", window, cx);
    }

    pub(crate) fn load_session_into_form(
        &mut self,
        session: &Session,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editing_session_id = Some(session.id.clone());
        self.ssh_auth_method = session.auth;
        self.session_protocol = session.protocol.clone();
        self.ssh_terminal_encoding = session.terminal_encoding;
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
        Self::set_input_value(
            &self.passphrase_input,
            session.passphrase.clone(),
            window,
            cx,
        );
        Self::set_input_value(
            &self.baud_rate_input,
            session.baud_rate.to_string(),
            window,
            cx,
        );
        self.ssh_proxy_type = if session.proxy_type.is_empty() {
            "none".to_string()
        } else {
            session.proxy_type.clone()
        };
        Self::set_input_value(
            &self.proxy_host_input,
            session.proxy_host.clone(),
            window,
            cx,
        );
        Self::set_input_value(
            &self.proxy_port_input,
            session
                .proxy_port
                .map(|p| p.to_string())
                .unwrap_or_default(),
            window,
            cx,
        );
        Self::set_input_value(
            &self.proxy_user_input,
            session.proxy_user.clone(),
            window,
            cx,
        );
        Self::set_input_value(
            &self.proxy_password_input,
            session.proxy_password.clone(),
            window,
            cx,
        );
    }

    pub(crate) fn pick_ssh_key_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let start_dir = directories::BaseDirs::new()
            .map(|d| d.home_dir().join(".ssh"))
            .unwrap_or_else(|| std::path::PathBuf::from("/"));

        let file_dialog = rfd::AsyncFileDialog::new()
            .set_directory(start_dir)
            .pick_file();

        cx.spawn_in(window, async move |this, cx| {
            if let Some(file) = file_dialog.await {
                let _ = gpui::AsyncWindowContext::update(cx, |window, cx| {
                    let _ = this.update(cx, |this, cx| {
                        Self::set_input_value(
                            &this.key_path_input,
                            file.path().to_string_lossy().to_string(),
                            window,
                            cx,
                        );
                    });
                });
            }
            Ok::<(), anyhow::Error>(())
        })
        .detach();
    }

    pub(crate) fn open_new_ssh_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reset_ssh_form(window, cx);
        self.show_ssh_dialog(window, cx);
    }

    pub(crate) fn edit_saved_session(
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

    pub(crate) fn clone_saved_session(
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
        self.editing_session_id = None;
        Self::set_input_value(
            &self.session_name_input,
            format!("{}-copy", session.name),
            window,
            cx,
        );
        self.show_ssh_dialog(window, cx);
    }

    pub(crate) fn terminal_cell_width(&self) -> f32 {
        (self.terminal_font_size * 0.646).max(6.0)
    }

    pub(crate) fn terminal_line_height(&self) -> f32 {
        (self.terminal_font_size * 1.385).max(self.terminal_font_size + 2.0)
    }

    pub(crate) fn change_terminal_font_size(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.terminal_font_size = (self.terminal_font_size + delta).clamp(10.0, 24.0);
        self.config.set_terminal_font_size(self.terminal_font_size);
        self.save_preferences_background();
        self.status = format!("terminal font size: {:.0}px", self.terminal_font_size).into();
        cx.notify();
    }

    pub(crate) fn change_ui_font_size(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.ui_font_size = (self.ui_font_size + delta).clamp(8.0, 24.0);
        self.config.set_ui_font_size(self.ui_font_size);
        self.save_preferences_background();
        Theme::global_mut(cx).font_size = px(self.ui_font_size);
        self.status = format!("UI font size: {:.0}px", self.ui_font_size).into();
        cx.notify();
    }

    pub(crate) fn change_ui_font_family(
        &mut self,
        family: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ui_font_family = family.into();
        self.config.set_ui_font_family(family);
        self.save_preferences_background();
        crate::app::theme::set_theme_font_names(Theme::global_mut(cx), &self.ui_font_family);
        cx.notify();
        window.refresh();
    }

    pub(crate) fn change_terminal_font_family(&mut self, family: &str, cx: &mut Context<Self>) {
        self.terminal_font_family = family.into();
        self.config.set_terminal_font_family(family);
        self.save_preferences_background();
        cx.notify();
    }

    pub(crate) fn change_cursor_style(
        &mut self,
        style: crate::session::config::CursorStyle,
        cx: &mut Context<Self>,
    ) {
        self.cursor_style = style;
        self.config.set_cursor_style(style);
        self.save_preferences_background();
        cx.notify();
    }

    pub(crate) fn reset_layout(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.config.set_layout_state(None, None, None);
        self.config.set_sftp_tree_panels(None);
        self.config.set_sftp_file_columns(None);
        self.config.set_sftp_file_columns_customized(false);
        self.save_preferences_background();

        self.is_layout_reset = true;
        self.workspace_panels = cx.new(|_| crate::app::resizable::ResizableState::default());
        self.body_panels = cx.new(|_| crate::app::resizable::ResizableState::default());
        self.sftp_tree_panels = cx.new(|_| crate::app::resizable::ResizableState::default());
        self.sftp_file_columns = cx.new(|_| crate::app::resizable::ResizableState::default());

        cx.notify();
    }

    pub(crate) fn set_ssh_auth_method(&mut self, method: AuthMethod, cx: &mut Context<Self>) {
        self.ssh_auth_method = method;
        if method == AuthMethod::Config {
            self.refresh_ssh_config();
            self.ssh_config_selected = None;
        }
        cx.notify();
    }

    pub(crate) fn set_session_protocol(&mut self, protocol: String, cx: &mut Context<Self>) {
        self.session_protocol = protocol;
        cx.notify();
    }

    pub(crate) fn set_ssh_terminal_encoding(
        &mut self,
        encoding: TextEncoding,
        cx: &mut Context<Self>,
    ) {
        if self.ssh_terminal_encoding != encoding {
            self.ssh_terminal_encoding = encoding;
            cx.notify();
        }
    }

    pub(crate) fn set_terminal_encoding(
        &mut self,
        tab_id: String,
        encoding: TextEncoding,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return;
        };
        if !matches!(tab.kind, TabKind::Local | TabKind::Ssh) || tab.text_encoding() == encoding {
            return;
        }

        tab.set_text_encoding(encoding);
        let saved_session = (tab.kind == TabKind::Ssh)
            .then(|| tab.session.clone())
            .flatten();
        if let Some(session) = saved_session.as_ref() {
            self.config.upsert(session.clone());
        }
        if self.config.remember_tabs() {
            self.capture_tabs_state();
        }
        if (saved_session.is_some() || self.config.remember_tabs())
            && let Err(err) = self.config.save()
        {
            tracing::warn!("failed to save terminal encoding: {err:#}");
        }

        self.status = t!("terminal_encoding_changed", encoding = encoding.label())
            .to_string()
            .into();
        cx.notify();
    }

    pub(crate) fn refresh_ssh_config(&mut self) {
        self.ssh_config_entries =
            crate::session::ssh_config::parse_ssh_config().unwrap_or_default();
    }

    pub(crate) fn select_ssh_config_entry(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ssh_config_selected = Some(index);
        if let Some(entry) = self.ssh_config_entries.get(index) {
            Self::set_input_value(
                &self.session_name_input,
                entry.host_alias.clone(),
                window,
                cx,
            );
            Self::set_input_value(&self.host_input, entry.hostname.clone(), window, cx);
            Self::set_input_value(&self.port_input, entry.port.to_string(), window, cx);
            // If no user specified in config, use current system user
            let user = if entry.user.is_empty() {
                std::env::var("USER")
                    .or_else(|_| std::env::var("USERNAME"))
                    .unwrap_or_else(|_| "root".to_string())
            } else {
                entry.user.clone()
            };
            Self::set_input_value(&self.user_input, user, window, cx);
            Self::set_input_value(
                &self.key_path_input,
                entry.identity_files.first().cloned().unwrap_or_default(),
                window,
                cx,
            );
            Self::set_input_value(&self.password_input, String::new(), window, cx);
            Self::set_input_value(&self.key_inline_input, String::new(), window, cx);
            Self::set_input_value(&self.passphrase_input, String::new(), window, cx);
            // Auto-connect on selection
            self.connect_ssh(window, cx);
        }
    }

    pub(crate) fn set_ssh_proxy_type(&mut self, proxy_type: String, cx: &mut Context<Self>) {
        self.ssh_proxy_type = proxy_type;
        cx.notify();
    }

    pub(crate) fn connect_saved_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        tracing::info!(
            "[ui] user clicked to connect saved session '{}'",
            session_id
        );
        let Some(session) = self.config.get(&session_id).cloned() else {
            self.status = "saved session not found".into();
            cx.notify();
            return;
        };
        if session.protocol == "serial" {
            self.open_serial_session(session, cx);
        } else {
            self.open_ssh_session(session, cx);
        }
    }

    pub(crate) fn selector_entries(&self) -> Vec<SelectorEntry> {
        let mut entries = vec![SelectorEntry::Local, SelectorEntry::NewSsh];
        entries.extend(
            self.config
                .sessions()
                .iter()
                .map(|session| SelectorEntry::Saved(session.id.clone())),
        );
        entries
    }

    pub(crate) fn default_selector_index(&self) -> usize {
        if self.config.sessions().is_empty() {
            0
        } else {
            2
        }
    }

    pub(crate) fn move_selector_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
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

    pub(crate) fn activate_selector_selection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entries = self.selector_entries();
        let Some(entry) = entries.get(self.selector_selection).cloned() else {
            return;
        };

        self.active_dialog = None;
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
        cx.notify();
    }

    pub(crate) fn on_selector_key_down(
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

    pub(crate) fn open_ssh_session(&mut self, session: Session, cx: &mut Context<Self>) {
        tracing::info!(
            "[session] opening ssh tab for session '{}' ({}@{})",
            session.name,
            session.user,
            session.host
        );
        let id = Uuid::new_v4().to_string();
        let backend_events =
            crate::terminal::GuardedBackendEventSender::new(self.events_tx.clone());
        let backend = ssh::spawn_ssh_terminal(
            self.runtime.handle(),
            id.clone(),
            session.clone(),
            DEFAULT_COLS,
            DEFAULT_ROWS,
            backend_events.clone(),
        );
        self.tabs.push(TerminalTab::new_ssh(
            id.clone(),
            &session,
            backend,
            backend_events,
        ));
        self.active_tab = Some(id.clone());
        self.connection_progress = Some(crate::app::ConnectionProgress {
            tab_id: id.clone(),
            title: rust_i18n::t!("connecting").into(),
            lines: vec![rust_i18n::t!("starting_connection").into()],
            failed: false,
        });
        self.pane_root = PaneLayout::Single(id.clone());
        self.focused_pane_path = vec![];
        let group_id = Uuid::new_v4().to_string();
        self.tab_groups.push(TabGroup {
            id: group_id.clone(),
            title: session.name.clone(),
            pane_root: PaneLayout::Single(id.clone()),
            sftp: Some(connecting_sftp_state()),
            sftp_tab_id: Some(id.clone()),
        });
        self.active_group = Some(group_id.clone());
        self.tabs_scroll_handle.scroll_to_item(self.tabs.len() - 1);
        if let Some(session_id) = self.active_session_id() {
            if let Some(index) = self
                .config
                .sessions()
                .iter()
                .position(|s| s.id == session_id)
            {
                self.saved_scroll_handle.scroll_to_item(index);
            }
        }
        cx.notify();
        let sftp_handle = crate::sftp::spawn_sftp(
            self.runtime.handle(),
            id.clone(),
            session,
            self.events_tx.clone(),
        );
        self.sftp_handles.insert(group_id.clone(), sftp_handle);
        self.active_tab = Some(id.clone());
        self.pending_sftp_path_sync = Some("/".into());
        self.status = "ssh tab opened".into();
        self.sync_system_tab_to_active_group();
        self.save_tabs_state_background();
        cx.notify();
    }

    pub(crate) fn open_serial_session(&mut self, session: Session, cx: &mut Context<Self>) {
        tracing::info!(
            "[session] opening serial tab for session '{}' ({})",
            session.name,
            session.host
        );
        let id = Uuid::new_v4().to_string();
        let backend_events =
            crate::terminal::GuardedBackendEventSender::new(self.events_tx.clone());
        let backend = crate::backend::serial::spawn_serial_client(
            self.runtime.handle(),
            id.clone(),
            session.clone(),
            backend_events.clone(),
        );
        self.tabs.push(TerminalTab::new_serial(
            id.clone(),
            &session,
            crate::terminal::BackendTx::Serial(backend),
            backend_events,
        ));
        self.active_tab = Some(id.clone());
        self.connection_progress = Some(crate::app::ConnectionProgress {
            tab_id: id.clone(),
            title: rust_i18n::t!("connecting").into(),
            lines: vec![rust_i18n::t!("starting_connection").into()],
            failed: false,
        });
        self.pane_root = PaneLayout::Single(id.clone());
        self.focused_pane_path = vec![];
        let group_id = Uuid::new_v4().to_string();
        self.tab_groups.push(TabGroup {
            id: group_id.clone(),
            title: session.name.clone(),
            pane_root: PaneLayout::Single(id.clone()),
            sftp: None,
            sftp_tab_id: None,
        });
        self.active_group = Some(group_id.clone());
        self.tabs_scroll_handle.scroll_to_item(self.tabs.len() - 1);
        self.sync_system_tab_to_active_group();
        if let Some(session_id) = self.active_session_id() {
            if let Some(index) = self
                .config
                .sessions()
                .iter()
                .position(|s| s.id == session_id)
            {
                self.saved_scroll_handle.scroll_to_item(index);
            }
        }
        self.status = "serial tab opened".into();
        self.save_tabs_state_background();
        cx.notify();
    }

    pub(crate) fn remove_saved_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.config.remove(&session_id);
        self.selected_connection_ids.remove(&session_id);
        self.selected_command_history
            .retain(|(id, _)| id != &session_id);
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save config: {err:#}");
        }
        self.status = "session removed".into();
        cx.notify();
    }

    pub(crate) fn toggle_connection_selection(
        &mut self,
        session_id: String,
        selected: bool,
        cx: &mut Context<Self>,
    ) {
        if selected {
            self.selected_connection_ids.insert(session_id);
        } else {
            self.selected_connection_ids.remove(&session_id);
        }
        cx.notify();
    }

    pub(crate) fn select_all_connections(
        &mut self,
        session_ids: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        self.selected_connection_ids.extend(session_ids);
        cx.notify();
    }

    pub(crate) fn remove_selected_sessions(&mut self, cx: &mut Context<Self>) {
        let selected = self
            .selected_connection_ids
            .iter()
            .filter(|id| self.config.get((*id).as_str()).is_some())
            .cloned()
            .collect::<Vec<_>>();
        if selected.is_empty() {
            self.selected_connection_ids.clear();
            cx.notify();
            return;
        }

        let count = selected.len();
        for session_id in selected {
            self.config.remove(&session_id);
            self.selected_command_history
                .retain(|(id, _)| id != &session_id);
        }
        self.selected_connection_ids.clear();
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save selected sessions: {err:#}");
        }
        self.status = t!("connections_deleted", count = count).into();
        cx.notify();
    }

    pub(crate) fn close_command_history(&mut self, cx: &mut Context<Self>) {
        let changed = self.show_command_history || !self.selected_command_history.is_empty();
        self.show_command_history = false;
        self.selected_command_history.clear();
        if changed {
            cx.notify();
        }
    }

    pub(crate) fn toggle_command_history_selection(
        &mut self,
        session_id: String,
        index: usize,
        selected: bool,
        cx: &mut Context<Self>,
    ) {
        let key = (session_id, index);
        if selected {
            self.selected_command_history.insert(key);
        } else {
            self.selected_command_history.remove(&key);
        }
        cx.notify();
    }

    pub(crate) fn set_command_history_selection(
        &mut self,
        entries: Vec<(String, usize)>,
        selected: bool,
        cx: &mut Context<Self>,
    ) {
        for entry in entries {
            if selected {
                self.selected_command_history.insert(entry);
            } else {
                self.selected_command_history.remove(&entry);
            }
        }
        cx.notify();
    }

    pub(crate) fn remove_selected_command_history(&mut self, cx: &mut Context<Self>) {
        let mut selected = self.selected_command_history.drain().collect::<Vec<_>>();
        selected.sort_by(|(left_session, left_index), (right_session, right_index)| {
            left_session
                .cmp(right_session)
                .then_with(|| right_index.cmp(left_index))
        });

        let mut count = 0;
        for (session_id, index) in selected {
            if self.config.remove_command_history(&session_id, index) {
                count += 1;
            }
        }
        if count > 0 {
            self.save_preferences_background();
            self.status = t!("commands_deleted", count = count).into();
        }
        cx.notify();
    }

    /// Retry a single disconnected tab by its ID.
    /// For SSH tabs: spawns a new SSH connection and restarts SFTP.
    /// For local tabs: spawns a new local shell.
    ///
    /// The existing `TerminalTab` (including its `term` scrollback history)
    /// is preserved — only the backend is swapped via `set_backend()`.
    pub(crate) fn retry_disconnected_tab(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        let Some(ix) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        if self.tabs[ix].connected || self.tabs[ix].disconnected_reason.is_none() {
            return;
        }

        let is_ssh = self.tabs[ix].session.is_some();
        let session = self.tabs[ix].session.clone();
        let cols = self.tabs[ix].cols;
        let rows = self.tabs[ix].rows;

        let backend_events = self.tabs[ix].advance_backend_events();
        // Advance the event generation before closing the old backend so its
        // final events cannot be mistaken for events from the replacement.
        self.tabs[ix].send_backend(BackendCommand::Close);

        if let Some(session) = session {
            let tab_kind = self.tabs[ix].kind;
            match tab_kind {
                crate::terminal::TabKind::Serial => {
                    let backend = crate::backend::serial::spawn_serial_client(
                        self.runtime.handle(),
                        tab_id.to_string(),
                        session.clone(),
                        backend_events.clone(),
                    );
                    self.tabs[ix].set_backend(crate::terminal::BackendTx::Serial(backend));
                }
                crate::terminal::TabKind::Ssh => {
                    let backend = ssh::spawn_ssh_terminal(
                        self.runtime.handle(),
                        tab_id.to_string(),
                        session.clone(),
                        cols,
                        rows,
                        backend_events.clone(),
                    );
                    self.tabs[ix].set_backend(backend);
                }
                _ => {}
            }
            self.tabs[ix].connected = false;
            self.tabs[ix].status = "connecting".into();
            self.tabs[ix].disconnected_reason = None;
            self.tabs[ix].terminal_title_received = false;

            if tab_kind == crate::terminal::TabKind::Ssh
                && self.active_tab.as_deref() == Some(tab_id)
            {
                self.restart_active_sftp();
            }
        } else {
            // Local tab: spawn new local shell
            let local_cwd = self.tabs[ix]
                .local_cwd
                .clone()
                .or_else(default_local_directory);
            match local::spawn_local_terminal_at(
                tab_id.to_string(),
                cols,
                rows,
                backend_events,
                local_cwd.as_deref(),
            ) {
                Ok(backend) => {
                    // Swap the backend — preserves terminal history.
                    self.tabs[ix].set_backend(backend);
                    self.tabs[ix].connected = true;
                    self.tabs[ix].status = "local shell".into();
                    self.tabs[ix].disconnected_reason = None;
                    self.tabs[ix].local_cwd = local_cwd;
                    // Resize the new PTY to match the pane dimensions.
                    self.tabs[ix].send_backend(BackendCommand::Resize { cols, rows });
                }
                Err(err) => {
                    self.status = format!("failed to reopen local terminal: {err:#}").into();
                    cx.notify();
                    return;
                }
            }
        }

        self.status = if is_ssh {
            "ssh tab retrying"
        } else {
            "local tab reopened"
        }
        .into();
        cx.notify();
    }

    #[allow(dead_code)]
    pub(crate) fn activate_tab(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let active_tab_changed = self.active_tab.as_deref() != Some(id.as_str());
        // Save current group state
        if let Some(group_id) = self.active_group.clone() {
            if let Some(group) = self.tab_groups.iter_mut().find(|g| g.id == group_id) {
                group.pane_root = self.pane_root.clone();
            }
        }
        self.active_tab = Some(id.clone());
        // Find which group this tab belongs to and restore its pane_root
        let tab_group = self
            .tab_groups
            .iter_mut()
            .find(|g| g.pane_root.contains(&id));
        if let Some(group) = tab_group {
            self.pane_root = group.pane_root.clone();
            self.active_group = Some(group.id.clone());
            // Focus the activated tab in the pane tree
            self.focus_pane_with_id(id.clone());
        } else {
            self.pane_root = PaneLayout::Single(id.clone());
            self.focused_pane_path = vec![];
        }
        if let Some(index) = self.tabs.iter().position(|t| t.id == id) {
            self.tabs_scroll_handle.scroll_to_item(index);
        }
        if self.tabs.iter().any(|t| t.id == id) {
            if let Some(session_id) = self.active_session_id() {
                if let Some(index) = self
                    .config
                    .sessions()
                    .iter()
                    .position(|s| s.id == session_id)
                {
                    self.saved_scroll_handle.scroll_to_item(index);
                }
            }
        }
        self.focus_handle.focus(window, cx);
        if !matches!(self.active_kind(), Some(TabKind::Ssh)) {
            self.show_command_history = false;
            self.selected_command_history.clear();
        }
        self.sync_sftp_to_active_tab();
        self.sync_system_tab_to_active_group();
        if active_tab_changed {
            self.prompt_active_ssh_reconnect_if_needed(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn close_tab(&mut self, id: String, cx: &mut Context<Self>) {
        self.handle_tab_close(id);
        cx.notify();
    }

    pub(crate) fn handle_tab_close(&mut self, id: String) {
        if self
            .connection_progress
            .as_ref()
            .is_some_and(|p| p.tab_id == id)
        {
            self.connection_progress = None;
        }
        let group_ix = self
            .tab_groups
            .iter()
            .position(|g| g.pane_root.contains(&id));
        let Some(ref group) = group_ix.map(|i| self.tab_groups[i].clone()) else {
            // Fallback: find and close individual tab
            tracing::info!(
                "[handle_tab_close] no group found for tab '{}', closing individually",
                id
            );
            if let Some(ix) = self.tabs.iter().position(|tab| tab.id == id) {
                self.tabs[ix].send_backend(BackendCommand::Close);
                self.tabs.remove(ix);
            }
            self.save_tabs_state_background();
            return;
        };

        let pane_ids = group.pane_root.tab_ids();
        let pane_ids_str = pane_ids.to_vec();
        let is_group_close = pane_ids.len() <= 1;
        tracing::info!(
            "[handle_tab_close] id='{}' group_panes={:?} is_group_close={}",
            id,
            pane_ids_str,
            is_group_close
        );

        let was_active = self.active_tab.as_deref() == Some(id.as_str());
        let mut next_active_id = None;
        if was_active {
            let tabs_in_group = group.pane_root.tab_ids();
            if let Some(pos) = tabs_in_group.iter().position(|&s| s == id.as_str()) {
                if pos > 0 {
                    next_active_id = Some(tabs_in_group[pos - 1].to_string());
                } else if pos + 1 < tabs_in_group.len() {
                    next_active_id = Some(tabs_in_group[pos + 1].to_string());
                }
            }
            if next_active_id.is_none() {
                // Find next group's active tab
                let all_groups = &self.tab_groups;
                if let Some(pos) = all_groups.iter().position(|g| g.id == group.id) {
                    if pos > 0 {
                        next_active_id = all_groups[pos - 1]
                            .pane_root
                            .tab_ids()
                            .first()
                            .copied()
                            .map(String::from);
                    } else if pos + 1 < all_groups.len() {
                        next_active_id = all_groups[pos + 1]
                            .pane_root
                            .tab_ids()
                            .first()
                            .copied()
                            .map(String::from);
                    }
                }
            }
        }
        if is_group_close {
            // Close all tabs in the group
            let tab_ids: Vec<String> = group
                .pane_root
                .tab_ids()
                .iter()
                .map(|s| s.to_string())
                .collect();
            for tab_id in &tab_ids {
                if let Some(ix) = self.tabs.iter().position(|tab| tab.id == *tab_id) {
                    self.tabs[ix].send_backend(BackendCommand::Close);
                    self.tabs.retain(|t| t.id != *tab_id);
                }
            }
            if let Some(handle) = self.sftp_handles.remove(&group.id) {
                handle.close();
            }
            self.tab_groups.remove(group_ix.unwrap());
            self.pane_root.remove_tab(&id);
        } else {
            // Just remove this tab from the group
            if let Some(ix) = self.tabs.iter().position(|tab| tab.id == id) {
                self.tabs[ix].send_backend(BackendCommand::Close);
                self.tabs.retain(|t| t.id != id);
            }
            if let Some(g) = self
                .tab_groups
                .iter_mut()
                .find(|g| g.pane_root.contains(&id))
            {
                g.pane_root.remove_tab(&id);
            }
            self.pane_root.remove_tab(&id);
            self.sync_pane_root_to_group();
        }

        if self.tabs.is_empty() || self.tab_groups.is_empty() {
            self.pane_root = PaneLayout::Single(String::new());
            self.focused_pane_path = vec![];
            self.active_tab = None;
            self.active_group = None;
            self.tab_groups.clear();
            self.tabs.clear();
            self.system_tab_id = None;
            self.cpu_history.clear();
            self.net_rx_history.clear();
            self.net_tx_history.clear();
            self.remote_processes.clear();
            self.remote_ports.clear();
            self.terminating_processes.clear();
            self.remote_process_status = None;
            self.remote_ports_status = None;
            self.remote_processes_in_flight = false;
            self.remote_ports_in_flight = false;
            self.expanded_process_pid = None;
            self.system_status = None;
            self.show_command_history = false;
            self.selected_command_history.clear();
            for (_, handle) in self.sftp_handles.drain() {
                handle.close();
            }
            self.save_tabs_state_background();
            return;
        }

        if was_active
            || self
                .active_tab
                .as_ref()
                .is_some_and(|active_id| !self.tabs.iter().any(|tab| &tab.id == active_id))
        {
            // Activate next available pane
            let new_id = next_active_id.or_else(|| {
                self.pane_root
                    .tab_ids()
                    .first()
                    .copied()
                    .map(String::from)
                    .or_else(|| self.tabs.first().map(|t| t.id.clone()))
            });
            if let Some(new_id) = new_id {
                self.active_tab = Some(new_id.clone());
                if let Some(g) = self
                    .tab_groups
                    .iter()
                    .find(|g| g.pane_root.contains(&new_id))
                {
                    self.active_group = Some(g.id.clone());
                    self.pane_root = g.pane_root.clone();
                }
                self.focus_pane_with_id(new_id);
            }
        } else {
            // Pane root structure may have changed (e.g. sibling removed), recalc path
            if let Some(active_id) = self.active_tab.clone() {
                self.focus_pane_with_id(active_id);
            }
        }
        if !matches!(self.active_kind(), Some(TabKind::Ssh)) {
            self.show_command_history = false;
            self.selected_command_history.clear();
        }
        self.sync_sftp_to_active_tab();
        self.sync_system_tab_to_active_group();
        self.save_tabs_state_background();
    }

    pub(crate) fn focus_terminal(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // If the search bar is visible and the click is inside it, let the
        // search bar handle the event instead of switching pane focus.
        if self.search_active {
            if let Some(bounds) = self.search_bar_bounds {
                if bounds.contains(&event.position) {
                    return;
                }
            }
        }
        self.focus_handle.focus(window, cx);
        // Check if click is in a different pane and focus it
        let click_pos = event.position;
        let current_active = self.active_tab.clone();
        let clicked_tab_id = self.terminal_bounds.iter().find_map(|(id, bounds)| {
            if bounds.contains(&click_pos) {
                Some(id.clone())
            } else {
                None
            }
        });
        if let Some(tab_id) = clicked_tab_id {
            if current_active.as_deref() != Some(tab_id.as_str()) {
                self.focus_pane_with_id(tab_id.clone());
                cx.notify();
            }
        }
        if event.button == MouseButton::Left {
            if event.modifiers.platform {
                if let Some((row, col, _side)) = self.terminal_grid_point_and_side(event.position) {
                    if let Some(snapshot) = self.active_snapshot() {
                        if let Some((url, _)) = crate::terminal::highlight::find_url_at_cell(
                            &snapshot.cells,
                            snapshot.rows,
                            row,
                            col,
                        ) {
                            let _ = open::that(&url);
                            return;
                        }
                    }
                }
            }
            if self.config.right_click_copy_paste() {
                if let Some(text) = self.active_terminal_selection_text() {
                    if !text.is_empty() {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                        if let Some(active_id) = &self.active_tab {
                            if let Some(tab) = self.tabs.iter_mut().find(|tab| &tab.id == active_id)
                            {
                                tab.clear_selection();
                            }
                        }
                    }
                }
            }
            self.begin_terminal_selection(event, cx);
        }
        cx.notify();
    }

    pub(crate) fn active_snapshot(&self) -> Option<RenderSnapshot> {
        self.active_tab
            .as_ref()
            .and_then(|id| self.tabs.iter().find(|t| &t.id == id))
            .map(|t| t.render_snapshot(self.config.keyword_highlight()))
    }

    pub(crate) fn active_kind(&self) -> Option<TabKind> {
        self.active_tab
            .as_ref()
            .and_then(|id| self.tabs.iter().find(|t| &t.id == id))
            .map(|tab| tab.kind)
    }

    pub(crate) fn active_title(&self) -> String {
        self.active_tab
            .as_ref()
            .and_then(|id| self.tabs.iter().find(|t| &t.id == id))
            .map(|t| t.title.clone())
            .unwrap_or_else(|| t!("idle_no_session").into())
    }

    pub(crate) fn active_ssh_session(&self) -> Option<(String, Session)> {
        let active_id = self.active_tab.as_ref()?;
        let tab = self.tabs.iter().find(|tab| &tab.id == active_id)?;
        if !tab.connected {
            return None;
        }
        Some((tab.id.clone(), tab.session.clone()?))
    }

    pub(crate) fn active_session_id(&self) -> Option<&str> {
        self.active_tab
            .as_ref()
            .and_then(|id| self.tabs.iter().find(|tab| &tab.id == id))
            .and_then(|tab| tab.session.as_ref())
            .map(|session| session.id.as_str())
    }

    pub(crate) fn session_detail(&self, session: &Session) -> String {
        if session.protocol == "serial" {
            format!("Serial: {}@{}", session.host, session.baud_rate)
        } else {
            format!("{}@{}:{}", session.user, session.host, session.port)
        }
    }

    pub(crate) fn split_current_pane(&mut self, direction: &str, cx: &mut Context<Self>) {
        tracing::info!(
            "[split] direction={} pane_root={:?} focused_path={:?} active_tab={:?} tabs={}",
            direction,
            self.pane_root,
            self.focused_pane_path,
            self.active_tab,
            self.tabs.len(),
        );
        let current_id = match self.pane_root.focused_tab_id(&self.focused_pane_path) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return,
        };
        // Find current tab to clone its type/session
        let current_tab = match self.tabs.iter().find(|t| t.id == current_id) {
            Some(tab) => tab,
            None => return,
        };
        let local_cwd = current_tab
            .local_cwd
            .clone()
            .or_else(default_local_directory);
        let new_id = Uuid::new_v4().to_string();
        let backend_events =
            crate::terminal::GuardedBackendEventSender::new(self.events_tx.clone());
        let mut tab = match current_tab.kind {
            TabKind::Local => {
                match local::spawn_local_terminal_at(
                    new_id.clone(),
                    DEFAULT_COLS,
                    DEFAULT_ROWS,
                    backend_events.clone(),
                    local_cwd.as_deref(),
                ) {
                    Ok(backend) => {
                        let title = local_cwd
                            .as_deref()
                            .map(compact_local_path)
                            .unwrap_or_else(initial_local_title);
                        let mut tab = TerminalTab::new_local(
                            new_id.clone(),
                            title,
                            backend,
                            backend_events.clone(),
                        );
                        tab.local_cwd = local_cwd;
                        tab
                    }
                    Err(err) => {
                        self.status = format!("failed to split: {err:#}").into();
                        cx.notify();
                        return;
                    }
                }
            }
            TabKind::Ssh => {
                let Some(session) = current_tab.session.clone() else {
                    self.status = "cannot split: no session info".into();
                    cx.notify();
                    return;
                };
                let backend = ssh::spawn_ssh_terminal(
                    self.runtime.handle(),
                    new_id.clone(),
                    session.clone(),
                    DEFAULT_COLS,
                    DEFAULT_ROWS,
                    backend_events.clone(),
                );
                TerminalTab::new_ssh(new_id.clone(), &session, backend, backend_events.clone())
            }
            TabKind::Serial => {
                let Some(session) = current_tab.session.clone() else {
                    self.status = "cannot split: no session info".into();
                    cx.notify();
                    return;
                };
                let backend = crate::backend::serial::spawn_serial_client(
                    self.runtime.handle(),
                    new_id.clone(),
                    session.clone(),
                    backend_events.clone(),
                );
                TerminalTab::new_serial(
                    new_id.clone(),
                    &session,
                    crate::terminal::BackendTx::Serial(backend),
                    backend_events,
                )
            }
        };
        tab.resize(DEFAULT_COLS, DEFAULT_ROWS);
        // Do NOT add to tab_groups — pane stays within the existing group
        self.tabs.push(tab);
        // Do NOT scroll tab bar or add tab bar entry

        let current_pane = PaneLayout::Single(current_id);
        let new_pane = PaneLayout::Single(new_id.clone());

        let split_layout = match direction {
            "left" | "right" => {
                let children = match direction {
                    "left" => vec![new_pane, current_pane],
                    _ => vec![current_pane, new_pane],
                };
                PaneLayout::Vertical(children, 0.5)
            }
            "up" | "down" => {
                let children = match direction {
                    "up" => vec![new_pane, current_pane],
                    _ => vec![current_pane, new_pane],
                };
                PaneLayout::Horizontal(children, 0.5)
            }
            _ => return,
        };

        self.pane_root
            .replace_at(&self.focused_pane_path, split_layout);
        self.sync_pane_root_to_group();
        // Update focused_pane_path: the new pane is at the indicated child index
        let parent_path = self.focused_pane_path.clone();
        let mut new_full_path = parent_path;
        if direction == "right" || direction == "down" {
            new_full_path.push(1);
        } else {
            new_full_path.push(0);
        }
        self.focused_pane_path = new_full_path;
        self.active_tab = Some(new_id);
        self.sync_sftp_to_active_tab();
        self.sync_system_tab_to_active_group();
        self.status = "pane split".into();
        tracing::info!(
            "[split] DONE: pane_root={:?} focused_path={:?} active_tab={:?} tabs={}",
            self.pane_root,
            self.focused_pane_path,
            self.active_tab,
            self.tabs.len(),
        );
        self.save_tabs_state_background();
        cx.notify();
    }

    pub(crate) fn focus_adjacent_pane(
        &mut self,
        direction: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.focused_pane_path.is_empty() {
            return;
        }
        let mut active_tab_changed = false;
        let path = self.focused_pane_path.clone();
        if let Some(new_path) = Self::find_adjacent_pane(&self.pane_root, &path, direction) {
            self.focused_pane_path = new_path;
            if let Some(id) = self.pane_root.focused_tab_id(&self.focused_pane_path) {
                let id_owned = id.to_string();
                let changed = self.active_tab.as_deref() != Some(id_owned.as_str());
                active_tab_changed = changed;
                self.active_tab = Some(id_owned);
                // Clear stale search state when switching to a different pane.
                if changed && self.search_active {
                    self.search_query.clear();
                    self.search_matches.clear();
                    self.search_current = 0;
                    self.search_target_tab = None;
                }
                if changed {
                    self.sync_sftp_to_active_tab();
                    self.sync_system_tab_to_active_group();
                }
            }
            cx.notify();
        }
        if active_tab_changed {
            self.prompt_active_ssh_reconnect_if_needed(window, cx);
        }
    }

    fn first_leaf_path(layout: &PaneLayout) -> Vec<usize> {
        match layout {
            PaneLayout::Single(_) => vec![],
            PaneLayout::Horizontal(children, _) | PaneLayout::Vertical(children, _) => {
                let mut path = vec![0];
                path.extend(Self::first_leaf_path(&children[0]));
                path
            }
        }
    }

    fn leaf_at_index(layout: &PaneLayout, index: usize) -> Vec<usize> {
        match layout {
            PaneLayout::Single(_) => vec![],
            PaneLayout::Horizontal(children, _) | PaneLayout::Vertical(children, _) => {
                if children.is_empty() {
                    return vec![];
                }
                let i = index.min(children.len() - 1);
                let mut path = vec![i];
                path.extend(Self::first_leaf_path(&children[i]));
                path
            }
        }
    }

    fn find_adjacent_pane(
        layout: &PaneLayout,
        path: &[usize],
        direction: &str,
    ) -> Option<Vec<usize>> {
        if path.is_empty() {
            return None;
        }
        match layout {
            PaneLayout::Single(_) => None,
            PaneLayout::Horizontal(children, _) | PaneLayout::Vertical(children, _) => {
                let is_horizontal = matches!(layout, PaneLayout::Horizontal(_, _));
                let idx = path[0];

                // Does this split level match the movement direction?
                let vert = direction == "up" || direction == "down";
                let horiz = direction == "left" || direction == "right";
                // PaneLayout::Horizontal renders as v_flex (vertical stack),
                // PaneLayout::Vertical renders as h_flex (horizontal row).
                // So for a Vertical (h_flex), h/l moves between children;
                // for a Horizontal (v_flex), j/k moves between children.
                let moves_in_this_split = (vert && is_horizontal) || (horiz && !is_horizontal);

                if path.len() == 1 {
                    // Direct child level
                    if moves_in_this_split {
                        let delta: i32 = if direction == "up" || direction == "left" {
                            -1
                        } else {
                            1
                        };
                        let new_idx = idx as i32 + delta;
                        if new_idx >= 0 && (new_idx as usize) < children.len() {
                            let mut path = vec![new_idx as usize];
                            path.extend(Self::first_leaf_path(&children[new_idx as usize]));
                            Some(path)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    // Recurse into child first
                    if let Some(mut child_path) =
                        Self::find_adjacent_pane(&children[idx], &path[1..], direction)
                    {
                        child_path.insert(0, idx);
                        Some(child_path)
                    } else if moves_in_this_split {
                        // Try sibling at this level
                        let delta: i32 = if direction == "up" || direction == "left" {
                            -1
                        } else {
                            1
                        };
                        let new_idx = idx as i32 + delta;
                        if new_idx >= 0 && (new_idx as usize) < children.len() {
                            let inner_idx = *path.get(1).unwrap_or(&0);
                            let mut path = vec![new_idx as usize];
                            path.extend(Self::leaf_at_index(
                                &children[new_idx as usize],
                                inner_idx,
                            ));
                            Some(path)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
            }
        }
    }

    pub(crate) fn activate_group(
        &mut self,
        group_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous_active_tab = self.active_tab.clone();
        // Save current group state
        if let Some(current_group_id) = self.active_group.clone() {
            if let Some(group) = self
                .tab_groups
                .iter_mut()
                .find(|g| g.id == current_group_id)
            {
                group.pane_root = self.pane_root.clone();
            }
        }
        // Load new group state
        if let Some(group) = self.tab_groups.iter().find(|g| g.id == group_id) {
            self.pane_root = group.pane_root.clone();
            self.active_group = Some(group_id);
            let ids = group.pane_root.tab_ids();
            if let Some(&first_id) = ids.first() {
                self.active_tab = Some(first_id.to_string());
                self.focus_pane_with_id(first_id.to_string());
            }
            self.focus_handle.focus(window, cx);
        }
        self.sync_sftp_to_active_tab();
        self.sync_system_tab_to_active_group();
        self.save_tabs_state_background();
        if previous_active_tab != self.active_tab {
            self.prompt_active_ssh_reconnect_if_needed(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn sync_pane_root_to_group(&mut self) {
        if let Some(group_id) = self.active_group.clone() {
            if let Some(group) = self.tab_groups.iter_mut().find(|g| g.id == group_id) {
                group.pane_root = self.pane_root.clone();
            }
        }
    }

    fn update_active_sftp_binding(&mut self, force: bool) {
        let Some(group_id) = self.active_group.clone() else {
            return;
        };
        let target = self.active_tab.as_ref().and_then(|active_id| {
            self.tabs
                .iter()
                .find(|tab| {
                    tab.id == *active_id && tab.kind == TabKind::Ssh && (tab.connected || force)
                })
                .and_then(|tab| tab.session.clone().map(|session| (tab.id.clone(), session)))
        });
        let target_tab_id = target.as_ref().map(|(tab_id, _)| tab_id.as_str());
        let current_tab_id = self
            .tab_groups
            .iter()
            .find(|group| group.id == group_id)
            .and_then(|group| group.sftp_tab_id.clone());
        let current_session_id = current_tab_id.as_ref().and_then(|tab_id| {
            self.tabs
                .iter()
                .find(|tab| tab.id == *tab_id)
                .and_then(|tab| tab.session.as_ref())
                .map(|session| session.id.clone())
        });
        let target_session_id = target.as_ref().map(|(_, session)| session.id.as_str());

        if !force {
            if current_tab_id.as_deref() == target_tab_id {
                return;
            }
            if current_session_id.as_deref() == target_session_id
                && target_session_id.is_some_and(|session_id| !session_id.is_empty())
                && self.sftp_handles.contains_key(&group_id)
            {
                return;
            }
        }

        if let Some(handle) = self.sftp_handles.remove(&group_id) {
            handle.close();
        }
        if let Some(group) = self
            .tab_groups
            .iter_mut()
            .find(|group| group.id == group_id)
        {
            group.sftp_tab_id = target.as_ref().map(|(tab_id, _)| tab_id.clone());
            group.sftp = target.as_ref().map(|_| connecting_sftp_state());
        }

        if let Some((tab_id, session)) = target {
            let handle = crate::sftp::spawn_sftp(
                self.runtime.handle(),
                tab_id,
                session,
                self.events_tx.clone(),
            );
            self.sftp_handles.insert(group_id, handle);
            self.pending_sftp_path_sync = Some("/".into());
            self.sftp_context_menu = None;
        }
    }

    pub(crate) fn sync_sftp_to_active_tab(&mut self) {
        self.update_active_sftp_binding(false);
    }

    pub(crate) fn restart_active_sftp(&mut self) {
        self.update_active_sftp_binding(true);
    }

    pub(crate) fn sync_system_tab_to_active_group(&mut self) {
        let active_ssh_tab = self.active_tab.as_ref().and_then(|id| {
            self.tabs
                .iter()
                .find(|tab| tab.id == *id && tab.kind == TabKind::Ssh)
        });
        let new_id = active_ssh_tab.map(|tab| tab.id.clone());
        let active_ssh_status = active_ssh_tab.and_then(|tab| {
            (!tab.connected).then(|| {
                tab.disconnected_reason
                    .clone()
                    .unwrap_or_else(|| tab.status.clone())
            })
        });

        if self.system_tab_id != new_id {
            self.system_tab_id = new_id;
            self.system = crate::system::SystemSnapshot::default();
            self.cpu_history.clear();
            self.net_rx_history.clear();
            self.net_tx_history.clear();
            self.remote_processes.clear();
            self.remote_ports.clear();
            self.terminating_processes.clear();
            self.remote_sample_in_flight = false;
            self.remote_processes_in_flight = false;
            self.remote_ports_in_flight = false;
            self.remote_process_status = None;
            self.remote_ports_status = None;
            self.expanded_process_pid = None;
            if let Some(status) = active_ssh_status {
                self.system_status = Some(status.clone().into());
                self.remote_process_status = Some(status.into());
            } else {
                self.system_status = None;
            }
            self.request_active_system_snapshot();
            if self.active_dialog == Some(crate::app::DialogKind::Processes) {
                self.request_active_process_snapshot();
            }
            if self.active_dialog == Some(crate::app::DialogKind::Ports) {
                self.request_active_port_snapshot();
            }
        }
    }

    pub(crate) fn start_drag_split(
        &mut self,
        parent_path: Vec<usize>,
        child_index: usize,
        event: &MouseDownEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.dragging_splitter = Some((parent_path, child_index));
        self.drag_split_origin = Some(event.position);
    }

    pub(crate) fn on_split_drag_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        let Some((ref parent_path, child_idx)) = self.dragging_splitter.clone() else {
            return;
        };
        let Some(origin) = self.drag_split_origin else {
            return;
        };
        let total = window.viewport_size();
        let is_horizontal = Self::is_layout_horizontal_at(&self.pane_root, parent_path);
        let delta: f32 = if is_horizontal {
            (event.position.y - origin.y).into()
        } else {
            (event.position.x - origin.x).into()
        };
        let total_size: f32 = if is_horizontal {
            total.height.into()
        } else {
            total.width.into()
        };
        if delta.abs() < 5.0 {
            return; // dead zone
        }
        let ratio_delta = delta / total_size;
        Self::adjust_split_ratio(&mut self.pane_root, parent_path, child_idx, ratio_delta);
        self.drag_split_origin = Some(event.position);
        self.sync_pane_root_to_group();
    }

    pub(crate) fn end_drag_split(&mut self) {
        self.dragging_splitter = None;
        self.drag_split_origin = None;
        self.save_tabs_state_background();
    }

    fn is_layout_horizontal_at(layout: &PaneLayout, path: &[usize]) -> bool {
        match (layout, path) {
            (PaneLayout::Horizontal(_, _), []) => true,
            (PaneLayout::Vertical(_, _), []) => false,
            (
                PaneLayout::Horizontal(children, _) | PaneLayout::Vertical(children, _),
                [first, rest @ ..],
            ) => children
                .get(*first)
                .is_some_and(|c| Self::is_layout_horizontal_at(c, rest)),
            _ => false,
        }
    }

    fn adjust_split_ratio(layout: &mut PaneLayout, path: &[usize], _child_idx: usize, delta: f32) {
        if let PaneLayout::Horizontal(children, ratio) | PaneLayout::Vertical(children, ratio) =
            layout
        {
            if path.is_empty() {
                *ratio = (*ratio + delta).clamp(0.1, 0.9);
            } else {
                let (&first, rest) = path.split_first().unwrap();
                if let Some(child) = children.get_mut(first) {
                    Self::adjust_split_ratio(child, rest, _child_idx, delta);
                }
            }
        }
    }

    pub(crate) fn focus_pane_with_id(&mut self, tab_id: String) {
        // Find the path to the given tab_id in the pane tree
        fn find_path(layout: &PaneLayout, target: &str, path: &mut Vec<usize>) -> bool {
            match layout {
                PaneLayout::Single(id) => id == target,
                PaneLayout::Horizontal(children, _) | PaneLayout::Vertical(children, _) => {
                    for (i, child) in children.iter().enumerate() {
                        path.push(i);
                        if find_path(child, target, path) {
                            return true;
                        }
                        path.pop();
                    }
                    false
                }
            }
        }
        let mut path = Vec::new();
        if find_path(&self.pane_root, &tab_id, &mut path) {
            let changed = self.active_tab.as_deref() != Some(tab_id.as_str());
            self.focused_pane_path = path;
            self.active_tab = Some(tab_id.clone());
            if let Some(title) = self
                .tabs
                .iter()
                .find(|tab| tab.id == tab_id && tab.kind == TabKind::Local)
                .map(|tab| tab.title.clone())
            {
                if let Some(group) = self
                    .tab_groups
                    .iter_mut()
                    .find(|group| group.pane_root.contains(&tab_id))
                {
                    group.title = title;
                }
            }
            // Clear stale search state when switching to a different pane.
            // The user can press Enter to re-search in the new pane.
            if changed && self.search_active {
                self.search_query.clear();
                self.search_matches.clear();
                self.search_current = 0;
                self.search_target_tab = None;
            }
            if changed {
                if !matches!(self.active_kind(), Some(TabKind::Ssh)) {
                    self.show_command_history = false;
                    self.selected_command_history.clear();
                }
                self.sync_sftp_to_active_tab();
                self.sync_system_tab_to_active_group();
            }
        }
    }
}

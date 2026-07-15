pub mod config;
pub mod ssh_config;
pub mod ssh_keys;
pub mod store;

use std::collections::{HashMap, HashSet};

use gpui::{
    AppContext as _, AnyWindowHandle, Context, Entity, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, SharedString, Window, px,
};
use gpui_component::{Theme, WindowExt as _, input::InputState};
use rust_i18n::t;
use uuid::Uuid;

use self::config::{AuthMethod, ManagedKey, Session};

use crate::{
    Ashell, PaneLayout, SelectorEntry, TabGroup,
    app::{
        IncomingTabDrag,
        constants::{DEFAULT_COLS, DEFAULT_ROWS},
        tab_drag::{DragTarget, DropIntent, TargetUpdate, reorder_index_at_x, should_offer_detach},
    },
    backend::{local, ssh},
    terminal::{BackendCommand, RenderSnapshot, TabKind, TerminalTab},
};

pub(crate) struct GroupTransfer {
    group: TabGroup,
    group_index: usize,
    tabs: Vec<(usize, TerminalTab)>,
    sftp_handles: HashMap<String, crate::sftp::SftpHandle>,
    route_ids: Vec<String>,
    active_tab: Option<String>,
    was_active_group: bool,
}

impl Ashell {
    pub(crate) fn open_local(&mut self, cx: &mut Context<Self>) {
        let id = Uuid::new_v4().to_string();
        let events = self.backend_events_sender(cx);
        match local::spawn_local_terminal(
            id.clone(),
            DEFAULT_COLS,
            DEFAULT_ROWS,
            events.clone(),
        ) {
            Ok(backend) => {
                let title = if cfg!(windows) { "PowerShell" } else { "Local" }.to_string();
                let mut tab = TerminalTab::new_local(id.clone(), title, backend, events);
                tab.resize(DEFAULT_COLS, DEFAULT_ROWS);
                self.tabs.push(tab);
                self.register_backend_route(id.clone(), cx);
                self.active_tab = Some(id.clone());
                self.pane_root = PaneLayout::Single(id.clone());
                self.focused_pane_path = vec![];
                let group_id = Uuid::new_v4().to_string();
                self.tab_groups.push(TabGroup {
                    id: group_id.clone(),
                    title: "Local".to_string(),
                    pane_root: PaneLayout::Single(id),
                    sftp: None,
                });
                self.active_group = Some(group_id);
                self.tabs_scroll_handle.scroll_to_item(self.tabs.len() - 1);
                self.status = "local terminal opened".into();
            }
            Err(err) => {
                self.status = format!("failed to open local terminal: {err:#}").into();
            }
        }
        cx.notify();
    }

    pub(crate) fn connect_ssh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        let existing_id = self.editing_session_id.clone();
        let existing_last_used = existing_id
            .as_deref()
            .and_then(|id| self.config.get(id))
            .and_then(|session| session.last_used.clone());

        let mut session = match self.ssh_auth_method {
            AuthMethod::Password => Session::password(host, port, user, password),
            AuthMethod::Key => {
                // If a managed key is selected, reference it by id.
                // The actual key content is resolved at connection time.
                if let Some(mk_id) = &self.managed_key_selected {
                    let mut s = Session::key(
                        host.clone(),
                        port,
                        user.clone(),
                        String::new(),
                        String::new(),
                        passphrase.clone(),
                    );
                    s.managed_key_id = Some(mk_id.clone());
                    s
                } else {
                    Session::key(host, port, user, key_path, key_inline, passphrase)
                }
            }
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
        self.config.upsert(session.clone());
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save config: {err:#}");
        }

        self.open_ssh_session(session, cx);
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
        self.managed_key_selected = None;
        self.using_custom_key_path = false;
        Self::set_input_value(&self.session_name_input, "", window, cx);
        Self::set_input_value(&self.host_input, "", window, cx);
        Self::set_input_value(&self.port_input, "22", window, cx);
        Self::set_input_value(&self.user_input, "root", window, cx);
        Self::set_input_value(&self.password_input, "", window, cx);
        Self::set_input_value(&self.key_path_input, "", window, cx);
        Self::set_input_value(&self.key_inline_input, "", window, cx);
        Self::set_input_value(&self.passphrase_input, "", window, cx);
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
        // Restore managed key selection or custom path mode.
        self.managed_key_selected = session.managed_key_id.clone();
        self.using_custom_key_path =
            session.auth == AuthMethod::Key && session.managed_key_id.is_none()
                && !session.private_key_path.is_empty();
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

        cx.spawn_in(window, async move |this, mut cx| {
            if let Some(file) = file_dialog.await {
                let _ = gpui::AsyncWindowContext::update(&mut cx, |window, cx| {
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

    // ── Managed SSH keys ────────────────────────────────────────────

    /// Open a file picker to import a private key into managed storage.
    /// Reads the file content, validates it, detects type + fingerprint,
    /// and saves a new `ManagedKey`.
    pub(crate) fn import_managed_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let start_dir = directories::BaseDirs::new()
            .map(|d| d.home_dir().join(".ssh"))
            .unwrap_or_else(|| std::path::PathBuf::from("/"));
        let file_dialog = rfd::AsyncFileDialog::new()
            .set_directory(start_dir)
            .pick_file();

        cx.spawn_in(window, async move |this, mut cx| {
            if let Some(file) = file_dialog.await {
                let path = file.path().to_path_buf();
                let content = std::fs::read_to_string(&path).unwrap_or_default();
                let file_name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("imported-key")
                    .to_string();
                let _ = gpui::AsyncWindowContext::update(&mut cx, |_window, cx| {
                    let _ = this.update(cx, |this, cx| {
                        this.finalize_key_import(content, file_name, cx);
                    });
                });
            }
            Ok::<(), anyhow::Error>(())
        })
        .detach();
    }

    /// Validate raw key content and save as a new ManagedKey.
    /// Reports errors via `self.status`.
    fn finalize_key_import(
        &mut self,
        content: String,
        default_name: String,
        cx: &mut Context<Self>,
    ) {
        let passphrase = self.passphrase_input.read(cx).value().to_string();
        let pass_opt = (!passphrase.is_empty()).then_some(passphrase.as_str());

        match crate::session::ssh_keys::validate_and_inspect(&content, pass_opt) {
            Ok((key_type, fingerprint)) => {
                // Check for duplicate fingerprint
                if self
                    .config
                    .managed_keys()
                    .iter()
                    .any(|k| k.fingerprint == fingerprint)
                {
                    self.status = t!("key_duplicate_fingerprint").to_string().into();
                    cx.notify();
                    return;
                }
                let key = ManagedKey {
                    id: Uuid::new_v4().to_string(),
                    name: default_name,
                    key_type,
                    fingerprint,
                    inline_content: content,
                    passphrase,
                    created_at: chrono::Local::now().timestamp(),
                };
                self.config.upsert_managed_key(key.clone());
                if let Err(err) = self.config.save() {
                    tracing::warn!("failed to save config: {err:#}");
                }
                self.managed_keys = self.config.managed_keys().to_vec();
                self.status = t!("key_imported").to_string().into();
                cx.notify();
            }
            Err(err) => {
                self.status = format!("{}: {err:#}", t!("key_import_failed")).into();
                cx.notify();
            }
        }
    }

    /// Rename a managed key.
    pub(crate) fn rename_managed_key(
        &mut self,
        key_id: String,
        new_name: String,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.config.managed_keys().iter().find(|k| k.id == key_id).cloned() else {
            return;
        };
        let mut updated = key;
        updated.name = new_name;
        self.config.upsert_managed_key(updated);
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save config: {err:#}");
        }
        self.managed_keys = self.config.managed_keys().to_vec();
        self.editing_managed_key_id = None;
        cx.notify();
    }

    /// Delete a managed key by id. Also clears the reference from any
    /// session that used it.
    pub(crate) fn delete_managed_key(&mut self, key_id: String, cx: &mut Context<Self>) {
        self.config.remove_managed_key(&key_id);
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save config: {err:#}");
        }
        self.managed_keys = self.config.managed_keys().to_vec();
        if self.managed_key_selected.as_deref() == Some(&key_id) {
            self.managed_key_selected = None;
        }
        cx.notify();
    }

    /// Select a managed key in the connection form.
    pub(crate) fn select_managed_key(&mut self, key_id: String, cx: &mut Context<Self>) {
        self.managed_key_selected = Some(key_id);
        self.using_custom_key_path = false;
        cx.notify();
    }

    /// Switch the connection form to use a custom key path (file picker).
    pub(crate) fn use_custom_key_path(&mut self, cx: &mut Context<Self>) {
        self.managed_key_selected = None;
        self.using_custom_key_path = true;
        cx.notify();
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
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save terminal font size: {err:#}");
        }
        self.status = format!("terminal font size: {:.0}px", self.terminal_font_size).into();
        cx.notify();
    }

    pub(crate) fn change_ui_font_size(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.ui_font_size = (self.ui_font_size + delta).clamp(8.0, 24.0);
        self.config.set_ui_font_size(self.ui_font_size);
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save UI font size: {err:#}");
        }
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
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save UI font family: {err:#}");
        }
        crate::app::theme::set_theme_font_names(Theme::global_mut(cx), &self.ui_font_family);
        cx.notify();
        window.refresh();
    }

    pub(crate) fn change_terminal_font_family(&mut self, family: &str, cx: &mut Context<Self>) {
        self.terminal_font_family = family.into();
        self.config.set_terminal_font_family(family);
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save terminal font family: {err:#}");
        }
        cx.notify();
    }

    pub(crate) fn change_cursor_style(
        &mut self,
        style: crate::session::config::CursorStyle,
        cx: &mut Context<Self>,
    ) {
        self.cursor_style = style;
        self.config.set_cursor_style(style);
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save cursor style: {err:#}");
        }
        cx.notify();
    }

    pub(crate) fn reset_layout(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.config.set_layout_state(None, None, None);
        let _ = self.config.save();

        self.is_layout_reset = true;
        self.workspace_panels = cx.new(|_| crate::app::resizable::ResizableState::default());
        self.body_panels = cx.new(|_| crate::app::resizable::ResizableState::default());

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
        self.open_ssh_session(session, cx);
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

    pub(crate) fn open_ssh_session(&mut self, mut session: Session, cx: &mut Context<Self>) {
        tracing::info!(
            "[session] opening ssh tab for session '{}' ({}@{})",
            session.name,
            session.user,
            session.host
        );

        // Resolve managed key reference: fill inline content from the
        // ManagedKey so the backend can authenticate without the original file.
        if let Some(mk_id) = &session.managed_key_id {
            if let Some(mk) = self.config.get_managed_key(mk_id) {
                session.private_key_inline = mk.inline_content.clone();
                session.private_key_path.clear();
                if session.passphrase.is_empty() {
                    session.passphrase = mk.passphrase.clone();
                }
            } else {
                tracing::warn!(
                    "[session] managed key '{}' not found, falling back to explicit key",
                    mk_id
                );
            }
        }

        let id = Uuid::new_v4().to_string();
        let events = self.backend_events_sender(cx);
        self.register_backend_route(id.clone(), cx);
        let backend = ssh::spawn_ssh_terminal(
            self.runtime.handle(),
            id.clone(),
            session.clone(),
            DEFAULT_COLS,
            DEFAULT_ROWS,
            events.clone(),
        );
        self.tabs.push(TerminalTab::new_ssh(
            id.clone(),
            &session,
            backend,
            events.clone(),
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
            sftp: Some(crate::terminal::SftpUiState {
                current_path: "/".into(),
                status: rust_i18n::t!("sftp_connecting").to_string(),
                entries: Vec::new(),
                selected_path: None,
                preview: None,
                selected_entries: std::collections::HashSet::new(),
                home_dir: "/".into(),
            }),
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
        self.register_backend_route(group_id.clone(), cx);
        let sftp_handle = crate::sftp::spawn_sftp(
            self.runtime.handle(),
            group_id.clone(),
            session,
            events,
        );
        self.sftp_handles.insert(group_id.clone(), sftp_handle);
        self.active_tab = Some(id.clone());
        self.pending_sftp_path_sync = Some("/".into());
        self.status = "ssh tab opened".into();
        cx.notify();
    }

    pub(crate) fn remove_saved_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.config.remove(&session_id);
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save config: {err:#}");
        }
        self.status = "session removed".into();
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
        let new_generation = self.tabs[ix].backend_generation + 1;
        let cols = self.tabs[ix].cols;
        let rows = self.tabs[ix].rows;
        let events = self.backend_events_sender(cx);
        self.register_backend_route(tab_id.to_string(), cx);

        // Close old backend (sends Close through the shared Arc<Mutex>)
        self.tabs[ix].send_backend(BackendCommand::Close);

        if let Some(session) = session {
            // SSH tab: spawn new SSH connection
            let backend = ssh::spawn_ssh_terminal(
                self.runtime.handle(),
                tab_id.to_string(),
                session.clone(),
                cols,
                rows,
                events.clone(),
            );

            // Swap the backend — the Term's internal listener shares the
            // same Arc<Mutex<BackendTx>>, so user input is automatically
            // routed to the new backend. Terminal history is preserved.
            self.tabs[ix].set_backend(backend);
            self.tabs[ix].connected = false;
            self.tabs[ix].status = "connecting".into();
            self.tabs[ix].disconnected_reason = None;
            self.tabs[ix].backend_generation = new_generation;
            self.tabs[ix].backend_initialized = false;

            // Restart SFTP for the group containing this tab
            if let Some(group) = self
                .tab_groups
                .iter()
                .find(|g| g.pane_root.contains(tab_id))
            {
                let group_id = group.id.clone();
                let group_session = self
                    .tabs
                    .iter()
                    .find(|t| group.pane_root.contains(&t.id) && t.session.is_some())
                    .and_then(|t| t.session.clone());

                if let Some(session) = group_session {
                    if let Some(old_handle) = self.sftp_handles.remove(&group_id) {
                        old_handle.close();
                    }
                    self.register_backend_route(group_id.clone(), cx);
                    let sftp_handle = crate::sftp::spawn_sftp(
                        self.runtime.handle(),
                        group_id.clone(),
                        session,
                        events.clone(),
                    );
                    self.sftp_handles.insert(group_id.clone(), sftp_handle);

                    if let Some(group) = self.tab_groups.iter_mut().find(|g| g.id == group_id) {
                        if let Some(sftp) = group.sftp.as_mut() {
                            sftp.status = rust_i18n::t!("sftp_connecting").to_string();
                        }
                    }
                }
            }
        } else {
            // Local tab: spawn new local shell
            match local::spawn_local_terminal(tab_id.to_string(), cols, rows, events) {
                Ok(backend) => {
                    // Swap the backend — preserves terminal history.
                    self.tabs[ix].set_backend(backend);
                    self.tabs[ix].connected = true;
                    self.tabs[ix].status = "local shell".into();
                    self.tabs[ix].disconnected_reason = None;
                    self.tabs[ix].backend_generation = new_generation;
                    self.tabs[ix].backend_initialized = false;
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
        self.sync_system_tab_to_active_group();
        cx.notify();
    }

    pub(crate) fn close_tab(&mut self, id: String, cx: &mut Context<Self>) {
        self.handle_tab_close(id);
        cx.notify();
    }

    pub(crate) fn handle_tab_close(&mut self, id: String) {
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
            return;
        };

        let pane_ids = group.pane_root.tab_ids();
        let pane_ids_str: Vec<&str> = pane_ids.iter().map(|s| *s).collect();
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
            self.system_status = None;
            for (_, handle) in self.sftp_handles.drain() {
                handle.close();
            }
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
        self.sync_system_tab_to_active_group();
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
            .map(TerminalTab::render_snapshot)
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
        format!("{}@{}:{}", session.user, session.host, session.port)
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
        let (current_kind, current_session) = match self.tabs.iter().find(|t| t.id == current_id) {
            Some(tab) => (tab.kind, tab.session.clone()),
            None => return,
        };
        let new_id = Uuid::new_v4().to_string();
        let events = self.backend_events_sender(cx);
        self.register_backend_route(new_id.clone(), cx);
        let mut tab = match current_kind {
            TabKind::Local => {
                match local::spawn_local_terminal(
                    new_id.clone(),
                    DEFAULT_COLS,
                    DEFAULT_ROWS,
                    events.clone(),
                ) {
                    Ok(backend) => TerminalTab::new_local(
                        new_id.clone(),
                        "Local".into(),
                        backend,
                        events.clone(),
                    ),
                    Err(err) => {
                        self.status = format!("failed to split: {err:#}").into();
                        cx.notify();
                        return;
                    }
                }
            }
            TabKind::Ssh => {
                let Some(session) = current_session else {
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
                    events.clone(),
                );
                let sftp_handle = crate::sftp::spawn_sftp(
                    self.runtime.handle(),
                    new_id.clone(),
                    session.clone(),
                    events.clone(),
                );
                self.sftp_handles.insert(new_id.clone(), sftp_handle);
                TerminalTab::new_ssh(new_id.clone(), &session, backend, events)
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
        self.status = "pane split".into();
        tracing::info!(
            "[split] DONE: pane_root={:?} focused_path={:?} active_tab={:?} tabs={}",
            self.pane_root,
            self.focused_pane_path,
            self.active_tab,
            self.tabs.len(),
        );
        cx.notify();
    }

    pub(crate) fn focus_adjacent_pane(&mut self, direction: &str) {
        if self.focused_pane_path.is_empty() {
            return;
        }
        let path = self.focused_pane_path.clone();
        if let Some(new_path) = Self::find_adjacent_pane(&self.pane_root, &path, direction) {
            self.focused_pane_path = new_path;
            if let Some(id) = self.pane_root.focused_tab_id(&self.focused_pane_path) {
                let id_owned = id.to_string();
                let changed = self.active_tab.as_deref() != Some(id_owned.as_str());
                self.active_tab = Some(id_owned);
                // Clear stale search state when switching to a different pane.
                if changed && self.search_active {
                    self.search_query.clear();
                    self.search_matches.clear();
                    self.search_current = 0;
                    self.search_target_tab = None;
                }
            }
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
        self.sync_system_tab_to_active_group();
        cx.notify();
    }

    pub(crate) fn sync_pane_root_to_group(&mut self) {
        if let Some(group_id) = self.active_group.clone() {
            if let Some(group) = self.tab_groups.iter_mut().find(|g| g.id == group_id) {
                group.pane_root = self.pane_root.clone();
            }
        }
    }

    pub(crate) fn sync_system_tab_to_active_group(&mut self) {
        let mut group_ssh_tabs = vec![];
        if let Some(group_id) = &self.active_group {
            if let Some(group) = self.tab_groups.iter().find(|g| g.id == *group_id) {
                let ids = group.pane_root.tab_ids();
                for id in ids {
                    if let Some(tab) = self.tabs.iter().find(|t| t.id == *id) {
                        if tab.kind == TabKind::Ssh && tab.connected {
                            group_ssh_tabs.push(tab.id.clone());
                        }
                    }
                }
            }
        }

        // Check if current system_tab_id is valid in this group
        let is_current_valid = self
            .system_tab_id
            .as_ref()
            .map_or(false, |id| group_ssh_tabs.contains(id));

        if !is_current_valid {
            let new_id = group_ssh_tabs.into_iter().next();
            if self.system_tab_id != new_id {
                self.system_tab_id = new_id;
                self.cpu_history.clear();
                self.net_rx_history.clear();
                self.net_tx_history.clear();
                self.remote_sample_in_flight = false;
                if self.system_tab_id.is_none() {
                    self.system_status = Some("monitored session closed".to_string().into());
                } else {
                    self.system_status = None;
                }
                self.request_active_system_snapshot();
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
                .map_or(false, |c| Self::is_layout_horizontal_at(c, rest)),
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
            self.active_tab = Some(tab_id);
            // Clear stale search state when switching to a different pane.
            // The user can press Enter to re-search in the new pane.
            if changed && self.search_active {
                self.search_query.clear();
                self.search_matches.clear();
                self.search_current = 0;
                self.search_target_tab = None;
            }
        }
    }

    // ─── Multi-window support ────────────────────────────────────────

    /// Open a new blank window.
    pub(crate) fn open_new_window(&mut self, cx: &mut Context<Self>) {
        crate::app::startup::open_new_window(None, Some(self.session_store.clone()), cx);
        self.status = "new window opened".into();
        cx.notify();
    }

    /// Detach the current active tab into a new window.
    /// For SSH tabs: extracts the session, closes the tab here, opens a new
    /// window that auto-connects to the same session.
    /// For Local tabs: opens a new window with a local terminal.
    pub(crate) fn detach_tab_to_new_window(&mut self, cx: &mut Context<Self>) {
        let Some(active_id) = self.active_tab.clone() else {
            return;
        };
        let Some(tab) = self.tabs.iter().find(|t| t.id == active_id) else {
            return;
        };

        let session = tab.session.clone();
        let is_local = tab.kind == TabKind::Local;

        // Window creation is the prepare step. Only commit the source close
        // after the target window has been constructed successfully.
        if is_local {
            crate::app::startup::open_new_window(None, Some(self.session_store.clone()), cx);
        } else if let Some(session) = session {
            crate::app::startup::open_new_window(
                Some(session),
                Some(self.session_store.clone()),
                cx,
            );
        } else {
            self.status = "cannot detach: SSH session information is missing".into();
            cx.notify();
            return;
        }
        self.close_tab(active_id, cx);

        self.status = "tab detached to new window".into();
        cx.notify();
    }

    /// Detach a complete tab group to a new window without recreating its
    /// terminal or SFTP backends. Window creation and route handoff form the
    /// prepare step; any failure restores the original group in place.
    pub(crate) fn detach_group_to_new_window(
        &mut self,
        group_id: &str,
        cx: &mut Context<Self>,
    ) {
        let source_owner_id = self.session_owner_id;
        let transfer = match self.take_group_transfer(group_id) {
            Ok(transfer) => transfer,
            Err(message) => {
                self.status = message.into();
                cx.notify();
                return;
            }
        };

        match crate::app::startup::open_new_window_with_group(
            transfer,
            source_owner_id,
            self.session_store.clone(),
            cx,
        ) {
            Ok(()) => {
                self.status = "tab group detached to new window".into();
            }
            Err((message, transfer)) => {
                self.restore_group_transfer(transfer, cx);
                self.status = format!("failed to detach tab group: {message}").into();
            }
        }
        cx.notify();
    }

    // ─── Tab drag, reorder, detach and merge ──────────────────────────

    /// Called on every root-level mouse move. Once the drag threshold is
    /// exceeded, the tab bar reorders within this window, any other source
    /// position detaches, and a hit on another window takes merge priority.
    pub(crate) fn on_tab_drag_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tab_drag.promote_if_needed(event.position, 5.0) {
            cx.notify();
        }
        if !self.tab_drag.is_dragging() {
            return;
        }

        let source_handle = window.window_handle();
        let source_entity = cx.entity();
        let screen_pos = Self::screen_position(window, event.position);
        self.update_tab_drag_merge_target(source_handle, source_entity, screen_pos, cx);

        let has_merge_target = self.tab_drag.merge_target().is_some();
        let reorder_index = if has_merge_target {
            None
        } else if self
            .tab_bar_bounds
            .as_ref()
            .is_some_and(|bounds| bounds.contains(&event.position))
        {
            let ordered_bounds = self
                .tab_groups
                .iter()
                .filter_map(|group| {
                    self.tab_group_bounds
                        .get(&group.id)
                        .copied()
                        .map(|bounds| (group.id.clone(), bounds))
                })
                .collect::<Vec<_>>();
            let dragged_group = self
                .tab_drag
                .dragging_group()
                .expect("dragging state must contain a group");
            reorder_index_at_x(dragged_group, event.position.x, &ordered_bounds)
        } else {
            None
        };
        let should_detach = should_offer_detach(
            self.tab_groups.len(),
            event.position,
            window.viewport_size(),
            self.tab_bar_bounds,
            has_merge_target,
        );

        let reorder_changed = self.tab_drag.set_reorder_index(reorder_index);
        let detach_changed = self.tab_drag.set_outside(should_detach);
        if reorder_changed || detach_changed {
            cx.notify();
        }
    }

    fn update_tab_drag_merge_target(
        &mut self,
        source_handle: AnyWindowHandle,
        source_entity: Entity<Ashell>,
        screen_pos: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let new_target = crate::app::find_window_at_screen_pos(&source_handle, screen_pos).map(
            |(window_id, entity, _)| DragTarget {
                window_id,
                payload: (window_id, entity),
            },
        );

        if let TargetUpdate::Changed { previous } = self.tab_drag.set_merge_target(new_target) {
            if let Some((_, previous)) = previous {
                previous.update(cx, |target, cx| {
                    target.incoming_tab_drag = None;
                    cx.notify();
                });
            }
            if let Some(target) = self.tab_drag.merge_target() {
                let (_, entity) = target.payload.clone();
                let incoming = IncomingTabDrag {
                    source_window: source_handle,
                    source: source_entity,
                    group_id: self
                        .tab_drag
                        .dragging_group()
                        .expect("dragging state must contain a group")
                        .to_string(),
                };
                entity.update(cx, |target, cx| {
                    target.incoming_tab_drag = Some(incoming);
                    cx.notify();
                });
            }
            cx.notify();
        }
    }

    fn prepare_tab_drag_release(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.tab_drag.is_dragging() {
            return;
        }

        let source_handle = window.window_handle();
        let screen_pos = Self::screen_position(window, event.position);
        self.update_tab_drag_merge_target(source_handle, cx.entity(), screen_pos, cx);

        let has_merge_target = self.tab_drag.merge_target().is_some();
        let reorder_index = if !has_merge_target
            && self
                .tab_bar_bounds
                .as_ref()
                .is_some_and(|bounds| bounds.contains(&event.position))
        {
            let ordered_bounds = self
                .tab_groups
                .iter()
                .filter_map(|group| {
                    self.tab_group_bounds
                        .get(&group.id)
                        .copied()
                        .map(|bounds| (group.id.clone(), bounds))
                })
                .collect::<Vec<_>>();
            self.tab_drag.dragging_group().and_then(|group_id| {
                reorder_index_at_x(group_id, event.position.x, &ordered_bounds)
            })
        } else {
            None
        };
        let should_detach = should_offer_detach(
            self.tab_groups.len(),
            event.position,
            window.viewport_size(),
            self.tab_bar_bounds,
            has_merge_target,
        );
        self.tab_drag.set_reorder_index(reorder_index);
        self.tab_drag.set_outside(should_detach);
    }

    /// Convert a window-local cursor position to a screen-space position by
    /// adding the source window's screen-space origin.
    fn screen_position(window: &Window, local: Point<Pixels>) -> Point<Pixels> {
        let origin = match window.window_bounds() {
            gpui::WindowBounds::Fullscreen(b)
            | gpui::WindowBounds::Maximized(b)
            | gpui::WindowBounds::Windowed(b) => b.origin,
        };
        Point::new(origin.x + local.x, origin.y + local.y)
    }

    /// Cancel a tab drag and clear the target window's incoming indicator.
    pub(crate) fn cancel_tab_drag(&mut self, cx: &mut Context<Self>) {
        if let Some((_, target)) = self.tab_drag.cancel() {
            target.update(cx, |target, cx| {
                target.incoming_tab_drag = None;
                cx.notify();
            });
        }
        cx.notify();
    }

    /// Finish a tab drag by moving the group to another existing window,
    /// detaching it to a new window, or cancelling a neutral release.
    pub(crate) fn on_tab_drag_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(incoming) = self.incoming_tab_drag.take() {
            let source_window = incoming.source_window;
            let source = incoming.source;
            let group_id = incoming.group_id;
            let target = cx.entity();
            let target_window = window.window_handle();
            window.defer(cx, move |_window, cx| {
                let _ = source.update(cx, |source, cx| {
                    source.finish_tab_drag_on_target(
                        group_id,
                        source_window,
                        target_window,
                        target,
                        cx,
                    );
                });
            });
            cx.notify();
            return;
        }

        self.prepare_tab_drag_release(event, window, cx);
        match self.tab_drag.finish() {
            DropIntent::Reorder { group_id, index } => {
                self.reorder_tab_group(&group_id, index, window, cx);
            }
            DropIntent::Merge {
                group_id,
                target: (target_window, target),
            } => {
                if self.commit_group_merge(group_id, target_window, target, cx)
                    && self.tab_groups.is_empty()
                {
                    window.remove_window();
                }
            }
            DropIntent::Detach { group_id } => {
                self.detach_group_to_new_window(&group_id, cx);
            }
            DropIntent::None | DropIntent::Cancelled => cx.notify(),
        }
    }

    fn finish_tab_drag_on_target(
        &mut self,
        group_id: String,
        source_window: AnyWindowHandle,
        target_window: AnyWindowHandle,
        target: Entity<Ashell>,
        cx: &mut Context<Self>,
    ) {
        let intent = self.tab_drag.finish();
        let valid_merge = matches!(
            intent,
            DropIntent::Merge {
                group_id: active_group_id,
                target: (active_target_window, _),
            } if active_group_id == group_id && active_target_window == target_window
        );
        if !valid_merge {
            target.update(cx, |target, cx| {
                target.incoming_tab_drag = None;
                cx.notify();
            });
            return;
        }

        if self.commit_group_merge(group_id, target_window, target, cx)
            && self.tab_groups.is_empty()
        {
            let _ = source_window.update(cx, |_, window, _| {
                window.remove_window();
            });
        }
    }

    fn commit_group_merge(
        &mut self,
        group_id: String,
        target_window: AnyWindowHandle,
        target: Entity<Ashell>,
        cx: &mut Context<Self>,
    ) -> bool {
        target.update(cx, |target, cx| {
            target.incoming_tab_drag = None;
            cx.notify();
        });
        let source_owner_id = self.session_owner_id;
        let merged = match self.take_group_transfer(&group_id) {
            Ok(transfer) => {
                let result = target.update(cx, |target, cx| {
                    target.receive_group_transfer(transfer, source_owner_id, cx)
                });
                match result {
                    Ok(()) => {
                        let focus_handle = target.read(cx).focus_handle.clone();
                        let _ = target_window.update(cx, |_, window, cx| {
                            window.activate_window();
                            window.focus(&focus_handle, cx);
                        });
                        self.status = "tab group moved into another window".into();
                        true
                    }
                    Err((message, transfer)) => {
                        self.restore_group_transfer(transfer, cx);
                        self.status = format!("failed to move tab group: {message}").into();
                        false
                    }
                }
            }
            Err(message) => {
                self.status = message.into();
                false
            }
        };
        cx.notify();
        merged
    }

    fn reorder_tab_group(
        &mut self,
        group_id: &str,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(current_index) = self
            .tab_groups
            .iter()
            .position(|group| group.id == group_id)
        else {
            self.status = "cannot reorder: source group no longer exists".into();
            cx.notify();
            return;
        };

        let group = self.tab_groups.remove(current_index);
        let target_index = index.min(self.tab_groups.len());
        let group_id = group.id.clone();
        self.tab_groups.insert(target_index, group);
        self.activate_group(group_id, window, cx);
        self.tabs_scroll_handle.scroll_to_item(target_index);
        self.status = "tab group reordered".into();
        window.activate_window();
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn take_group_transfer(&mut self, group_id: &str) -> Result<GroupTransfer, String> {
        let group_index = self
            .tab_groups
            .iter()
            .position(|group| group.id == group_id)
            .ok_or_else(|| "cannot move: source group no longer exists".to_string())?;
        let group = self.tab_groups[group_index].clone();
        let tab_ids = group
            .pane_root
            .tab_ids()
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        if tab_ids.is_empty() || tab_ids.iter().any(String::is_empty) {
            return Err("cannot move: source group has no terminal panes".to_string());
        }
        let tab_id_set = tab_ids.iter().map(String::as_str).collect::<HashSet<_>>();
        if tab_id_set.len() != tab_ids.len() {
            return Err("cannot move: source group contains duplicate terminal ids".to_string());
        }
        if tab_ids
            .iter()
            .any(|tab_id| !self.tabs.iter().any(|tab| tab.id == *tab_id))
        {
            return Err("cannot move: a source terminal no longer exists".to_string());
        }

        let was_active_group = self.active_group.as_deref() == Some(group_id);
        let active_tab = self
            .active_tab
            .clone()
            .filter(|tab_id| tab_id_set.contains(tab_id.as_str()));
        let mut tabs = Vec::with_capacity(tab_ids.len());
        let mut remaining_tabs = Vec::with_capacity(self.tabs.len() - tab_ids.len());
        for (index, tab) in std::mem::take(&mut self.tabs).into_iter().enumerate() {
            if tab_id_set.contains(tab.id.as_str()) {
                tabs.push((index, tab));
            } else {
                remaining_tabs.push(tab);
            }
        }
        self.tabs = remaining_tabs;

        let mut sftp_handles = HashMap::new();
        let handle_ids = self
            .sftp_handles
            .keys()
            .filter(|id| id.as_str() == group_id || tab_id_set.contains(id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        for handle_id in handle_ids {
            if let Some(handle) = self.sftp_handles.remove(&handle_id) {
                sftp_handles.insert(handle_id, handle);
            }
        }

        self.tab_groups.remove(group_index);
        let mut route_ids = tab_ids;
        route_ids.extend(sftp_handles.keys().cloned());
        route_ids.sort();
        route_ids.dedup();

        if was_active_group {
            self.activate_after_group_extraction(group_index);
        } else {
            self.sync_system_tab_to_active_group();
        }
        Ok(GroupTransfer {
            group,
            group_index,
            tabs,
            sftp_handles,
            route_ids,
            active_tab,
            was_active_group,
        })
    }

    fn activate_after_group_extraction(&mut self, removed_index: usize) {
        if self.tab_groups.is_empty() {
            self.pane_root = PaneLayout::Single(String::new());
            self.focused_pane_path.clear();
            self.active_tab = None;
            self.active_group = None;
            self.sync_system_tab_to_active_group();
            return;
        }

        let next_index = removed_index.min(self.tab_groups.len() - 1);
        let next_group = &self.tab_groups[next_index];
        let next_group_id = next_group.id.clone();
        let next_layout = next_group.pane_root.clone();
        let next_tab = next_layout.tab_ids().first().copied().map(str::to_string);
        self.active_group = Some(next_group_id);
        self.pane_root = next_layout;
        self.focused_pane_path.clear();
        self.active_tab = next_tab.clone();
        if let Some(tab_id) = next_tab {
            self.focus_pane_with_id(tab_id);
        }
        self.sync_system_tab_to_active_group();
    }

    fn restore_group_transfer(
        &mut self,
        mut transfer: GroupTransfer,
        cx: &mut Context<Self>,
    ) {
        let owner_id = self.session_owner_id;
        self.session_store.update(cx, |store, _| {
            if !store.move_event_routes(&transfer.route_ids, owner_id, owner_id) {
                for route_id in &transfer.route_ids {
                    store.register_event_route(route_id.clone(), owner_id);
                }
            }
        });

        let group_index = transfer.group_index.min(self.tab_groups.len());
        let group_id = transfer.group.id.clone();
        let group_layout = transfer.group.pane_root.clone();
        self.tab_groups.insert(group_index, transfer.group);
        transfer.tabs.sort_by_key(|(index, _)| *index);
        for (index, tab) in transfer.tabs {
            self.tabs.insert(index.min(self.tabs.len()), tab);
        }
        self.sftp_handles.extend(transfer.sftp_handles);

        if transfer.was_active_group {
            self.active_group = Some(group_id);
            self.pane_root = group_layout;
            self.focused_pane_path.clear();
            self.active_tab = transfer
                .active_tab
                .or_else(|| self.pane_root.tab_ids().first().copied().map(str::to_string));
            if let Some(tab_id) = self.active_tab.clone() {
                self.focus_pane_with_id(tab_id);
            }
        }
        self.sync_system_tab_to_active_group();
        cx.notify();
    }

    /// Receive an intact group from another window without recreating any
    /// terminal or SFTP backend. The group remains a separate top-level tab
    /// because `TabGroup` owns a single SFTP UI state.
    pub(crate) fn receive_group_transfer(
        &mut self,
        transfer: GroupTransfer,
        source_owner_id: crate::session::store::WindowOwnerId,
        cx: &mut Context<Self>,
    ) -> Result<(), (String, GroupTransfer)> {
        let tab_ids = transfer
            .tabs
            .iter()
            .map(|(_, tab)| tab.id.as_str())
            .collect::<HashSet<_>>();
        if tab_ids.len() != transfer.tabs.len()
            || transfer
                .group
                .pane_root
                .tab_ids()
                .iter()
                .any(|tab_id| !tab_ids.contains(*tab_id))
        {
            return Err((
                "transfer payload does not match the group layout".to_string(),
                transfer,
            ));
        }
        if self
            .tab_groups
            .iter()
            .any(|group| group.id == transfer.group.id)
        {
            return Err(("target already contains this group".to_string(), transfer));
        }
        if self.tabs.iter().any(|tab| tab_ids.contains(tab.id.as_str())) {
            return Err((
                "target already contains one of the transferred terminals".to_string(),
                transfer,
            ));
        }
        if transfer
            .sftp_handles
            .keys()
            .any(|handle_id| self.sftp_handles.contains_key(handle_id))
        {
            return Err((
                "target already contains one of the transferred SFTP handles".to_string(),
                transfer,
            ));
        }

        let target_owner_id = self.session_owner_id;
        let routes_moved = self.session_store.update(cx, |store, _| {
            store.move_event_routes(
                &transfer.route_ids,
                source_owner_id,
                target_owner_id,
            )
        });
        if !routes_moved {
            return Err((
                "backend event routes changed before the move could commit".to_string(),
                transfer,
            ));
        }

        let GroupTransfer {
            group,
            tabs,
            sftp_handles,
            active_tab,
            ..
        } = transfer;
        let group_id = group.id.clone();
        let group_layout = group.pane_root.clone();
        let fallback_tab = group_layout
            .tab_ids()
            .first()
            .copied()
            .map(str::to_string);
        self.tabs.extend(tabs.into_iter().map(|(_, tab)| tab));
        self.sftp_handles.extend(sftp_handles);
        self.tab_groups.push(group);
        self.active_group = Some(group_id);
        self.pane_root = group_layout;
        self.focused_pane_path.clear();
        self.active_tab = active_tab.or(fallback_tab);
        if let Some(tab_id) = self.active_tab.clone() {
            self.focus_pane_with_id(tab_id);
        }
        self.sync_system_tab_to_active_group();
        self.status = "tab group moved from another window".into();
        cx.notify();
        Ok(())
    }
}

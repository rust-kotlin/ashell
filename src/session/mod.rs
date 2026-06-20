pub mod config;

use gpui::{
    App, AppContext as _, Context, Entity, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, SharedString, Window, px,
};
use gpui_component::{Theme, WindowExt as _, input::InputState};
use rust_i18n::t;
use uuid::Uuid;

use self::config::{AuthMethod, Session};

use crate::{
    Ashell, ConnectionProgress, PaneLayout, SelectorEntry, TabGroup,
    app::constants::{
        SIDEBAR_WIDTH, TAB_BAR_HEIGHT, TERMINAL_PADDING_X, TERMINAL_PADDING_Y,
    },
    backend::{local, ssh},
    terminal::{BackendCommand, RenderSnapshot, TabKind, TerminalTab},
};

impl Ashell {
    fn configured_terminal_size(&self) -> (u16, u16) {
        self.config.terminal_size()
    }

    pub(crate) fn open_local(&mut self, cx: &mut Context<Self>) {
        let id = Uuid::new_v4().to_string();
        let (cols, rows) = self.configured_terminal_size();
        match local::spawn_local_terminal(
            id.clone(),
            cols,
            rows,
            self.events_tx.clone(),
        ) {
            Ok(backend) => {
                let title = if cfg!(windows) { "PowerShell" } else { "Local" }.to_string();
                self.tabs.push(TerminalTab::new_local(
                    id.clone(),
                    title,
                    backend,
                    self.events_tx.clone(),
                    cols,
                    rows,
                ));
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
                Session::key(host, port, user, key_path, key_inline, passphrase)
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
        Self::set_input_value(&self.session_name_input, "", window, cx);
        Self::set_input_value(&self.host_input, "", window, cx);
        Self::set_input_value(&self.port_input, "22", window, cx);
        Self::set_input_value(&self.user_input, "root", window, cx);
        Self::set_input_value(&self.password_input, "", window, cx);
        Self::set_input_value(&self.key_path_input, "", window, cx);
        Self::set_input_value(&self.key_inline_input, "", window, cx);
        Self::set_input_value(&self.passphrase_input, "", window, cx);
    }

    pub(crate) fn load_session_into_form(
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
        Self::set_input_value(
            &self.passphrase_input,
            session.passphrase.clone(),
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

    pub(crate) fn change_terminal_cols(&mut self, delta: i16, cx: &mut Context<Self>) {
        let (cols, rows) = self.configured_terminal_size();
        let next_cols = (cols as i16 + delta).clamp(40, 240) as u16;
        self.config.set_terminal_size(next_cols, rows);
        self.config.clear_window_bounds();
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save terminal size: {err:#}");
        }
        self.status = format!("terminal size: {}x{}", next_cols, rows).into();
        cx.notify();
    }

    pub(crate) fn change_terminal_rows(&mut self, delta: i16, cx: &mut Context<Self>) {
        let (cols, rows) = self.configured_terminal_size();
        let next_rows = (rows as i16 + delta).clamp(12, 80) as u16;
        self.config.set_terminal_size(cols, next_rows);
        self.config.clear_window_bounds();
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save terminal size: {err:#}");
        }
        self.status = format!("terminal size: {}x{}", cols, next_rows).into();
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

    pub(crate) fn reset_layout(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.config.set_layout_state(None, None, None);
        let _ = self.config.save();

        self.is_layout_reset = true;
        self.workspace_panels = cx.new(|_| gpui_component::resizable::ResizableState::default());
        self.body_panels = cx.new(|_| gpui_component::resizable::ResizableState::default());

        cx.notify();
    }

    pub(crate) fn set_ssh_auth_method(&mut self, method: AuthMethod, cx: &mut Context<Self>) {
        self.ssh_auth_method = method;
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

    pub(crate) fn open_ssh_session(&mut self, session: Session, cx: &mut Context<Self>) {
        tracing::info!(
            "[session] opening ssh tab for session '{}' ({}@{})",
            session.name,
            session.user,
            session.host
        );
        let id = Uuid::new_v4().to_string();
        let (cols, rows) = self.configured_terminal_size();
        let backend = ssh::spawn_ssh_terminal(
            self.runtime.handle(),
            id.clone(),
            session.clone(),
            cols,
            rows,
            self.events_tx.clone(),
        );
        self.tabs.push(TerminalTab::new_ssh(
            id.clone(),
            &session,
            backend,
            self.events_tx.clone(),
            cols,
            rows,
        ));
        self.active_tab = Some(id.clone());
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
        let sftp_handle = crate::sftp::spawn_sftp(
            self.runtime.handle(),
            group_id.clone(),
            session,
            self.events_tx.clone(),
        );
        self.sftp_handles.insert(group_id.clone(), sftp_handle);
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

    pub(crate) fn remove_saved_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.config.remove(&session_id);
        if let Err(err) = self.config.save() {
            tracing::warn!("failed to save config: {err:#}");
        }
        self.status = "session removed".into();
        cx.notify();
    }

    pub(crate) fn retry_connection_progress(&mut self, cx: &mut Context<Self>) {
        let Some(progress) = self.connection_progress.clone() else {
            return;
        };
        self.connection_progress = None;
        let mut groups_to_restart_sftp = std::collections::HashSet::new();

        let mut retry_tabs = Vec::new();
        for (ix, tab) in self.tabs.iter().enumerate() {
            if !tab.connected && tab.session.is_some() {
                retry_tabs.push((ix, tab.id.clone(), tab.session.clone().unwrap()));
            }
        }

        if retry_tabs.is_empty() {
            cx.notify();
            return;
        }

        let (cols, rows) = self.configured_terminal_size();
        for (ix, tab_id, session) in retry_tabs {
            // Close old backend
            self.tabs[ix].backend.send(BackendCommand::Close);

            // Spawn new backend
            let backend = ssh::spawn_ssh_terminal(
                self.runtime.handle(),
                tab_id.clone(),
                session.clone(),
                cols,
                rows,
                self.events_tx.clone(),
            );

            // Replace tab state in-place to reuse the UI component
            self.tabs[ix] = TerminalTab::new_ssh(
                tab_id.clone(),
                &session,
                backend,
                self.events_tx.clone(),
                cols,
                rows,
            );

            // Find group to restart SFTP
            if let Some(group) = self
                .tab_groups
                .iter()
                .find(|g| g.pane_root.contains(&tab_id))
            {
                groups_to_restart_sftp.insert(group.id.clone());
            }
        }

        // Restart SFTP for affected groups
        for group_id in groups_to_restart_sftp {
            if let Some(group) = self.tab_groups.iter_mut().find(|g| g.id == group_id) {
                // Use the session of any tab in that group
                let group_session = self
                    .tabs
                    .iter()
                    .find(|t| group.pane_root.contains(&t.id) && t.session.is_some())
                    .and_then(|t| t.session.clone());

                if let Some(session) = group_session {
                    if let Some(old_handle) = self.sftp_handles.remove(&group.id) {
                        old_handle.close();
                    }
                    let sftp_handle = crate::sftp::spawn_sftp(
                        self.runtime.handle(),
                        group.id.clone(),
                        session,
                        self.events_tx.clone(),
                    );
                    self.sftp_handles.insert(group.id.clone(), sftp_handle);

                    if let Some(sftp) = group.sftp.as_mut() {
                        sftp.status = rust_i18n::t!("sftp_connecting").to_string();
                    }
                }
            }
        }

        self.connection_progress = Some(ConnectionProgress {
            tab_id: progress.tab_id.clone(),
            title: t!("connecting").into(),
            lines: vec![t!("starting_connection").into()],
            failed: false,
        });
        self.status = "ssh tabs retrying".into();
        cx.notify();
    }

    pub(crate) fn cancel_connection_progress(&mut self, cx: &mut Context<Self>) {
        if self.connection_progress.is_none() {
            return;
        }
        self.connection_progress = None;
        let tabs_to_close: Vec<_> = self
            .tabs
            .iter()
            .filter(|tab| !tab.connected && tab.session.is_some())
            .map(|tab| tab.id.clone())
            .collect();
        for id in tabs_to_close {
            self.handle_tab_close(id);
        }
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
                self.tabs[ix].backend.send(BackendCommand::Close);
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
                    self.tabs[ix].backend.send(BackendCommand::Close);
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
                self.tabs[ix].backend.send(BackendCommand::Close);
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

    pub(crate) fn sync_terminal_size(&mut self, window: &Window, cx: &App) {
        let viewport = window.viewport_size();
        let sidebar_width = self
            .workspace_panels
            .read(cx)
            .sizes()
            .first()
            .map(|size| size.as_f32())
            .unwrap_or(SIDEBAR_WIDTH);

        // Use the whole terminal panel bounds for PTY sizing.
        // Individual pane bounds are smaller when split, so using them here
        // would shrink the root PTY dimensions a second time.
        let (width, height) = if let Some(bounds) = self.terminal_panel_bounds {
            (bounds.size.width.as_f32(), bounds.size.height.as_f32())
        } else {
            let terminal_height = self
                .body_panels
                .read(cx)
                .sizes()
                .first()
                .map(|size| size.as_f32())
                .unwrap_or(viewport.height.as_f32() - TAB_BAR_HEIGHT - 248.0);
            (
                (viewport.width.as_f32() - sidebar_width - TERMINAL_PADDING_X - 8.0)
                    .max(self.terminal_cell_width()),
                (terminal_height - TERMINAL_PADDING_Y).max(self.terminal_line_height()),
            )
        };
        let total_cols = (width / self.terminal_cell_width()).floor().max(1.0) as u16;
        let total_rows = (height / self.terminal_line_height()).floor().max(1.0) as u16;

        Self::resize_pane_tree(&mut self.tabs, &self.pane_root, total_cols, total_rows);
    }

    fn resize_pane_tree(tabs: &mut [TerminalTab], layout: &PaneLayout, cols: u16, rows: u16) {
        match layout {
            PaneLayout::Single(id) => {
                if let Some(tab) = tabs.iter_mut().find(|t| t.id == *id) {
                    tab.resize(cols.max(1), rows.max(1));
                }
            }
            PaneLayout::Horizontal(children, _) => {
                let n = children.len() as u16;
                let each_rows = (rows / n).max(1);
                for child in children {
                    Self::resize_pane_tree(tabs, child, cols, each_rows);
                }
            }
            PaneLayout::Vertical(children, _) => {
                let n = children.len() as u16;
                let each_cols = (cols / n).max(1);
                for child in children {
                    Self::resize_pane_tree(tabs, child, each_cols, rows);
                }
            }
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
        let new_id = Uuid::new_v4().to_string();
        let (cols, rows) = self.configured_terminal_size();
        let mut tab = match current_tab.kind {
            TabKind::Local => {
                match local::spawn_local_terminal(
                    new_id.clone(),
                    cols,
                    rows,
                    self.events_tx.clone(),
                ) {
                    Ok(backend) => TerminalTab::new_local(
                        new_id.clone(),
                        "Local".into(),
                        backend,
                        self.events_tx.clone(),
                        cols,
                        rows,
                    ),
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
                    cols,
                    rows,
                    self.events_tx.clone(),
                );
                let sftp_handle = crate::sftp::spawn_sftp(
                    self.runtime.handle(),
                    new_id.clone(),
                    session.clone(),
                    self.events_tx.clone(),
                );
                self.sftp_handles.insert(new_id.clone(), sftp_handle);
                TerminalTab::new_ssh(
                    new_id.clone(),
                    &session,
                    backend,
                    self.events_tx.clone(),
                    cols,
                    rows,
                )
            }
        };
        tab.resize(cols, rows);
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
                self.active_tab = Some(id.to_string());
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
            self.focused_pane_path = path;
            self.active_tab = Some(tab_id);
        }
    }
}

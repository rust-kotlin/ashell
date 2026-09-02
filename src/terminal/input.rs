use std::ops::Range;

use alacritty_terminal::index::Side;
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::{TermMode, cell::Flags};
use gpui::{
    ClipboardItem, Context, Focusable as _, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollDelta, ScrollWheelEvent, Window, px,
};

use crate::{
    Ashell, TerminalBacktabKey, TerminalTabKey,
    terminal::{BackendCommand, encode_key},
};

thread_local! {
    static LAST_DRAG_SCROLL: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
}

const TERMINAL_ZOOM_PIXEL_STEP: f32 = 20.0;

fn terminal_zoom_steps(delta: &ScrollDelta, accumulator: &mut f32) -> i32 {
    match delta {
        ScrollDelta::Lines(point) => {
            *accumulator = 0.0;
            point.y.signum() as i32
        }
        ScrollDelta::Pixels(point) => {
            let delta_y = point.y.as_f32();
            if delta_y == 0.0 {
                return 0;
            }
            if *accumulator != 0.0 && (*accumulator).signum() != delta_y.signum() {
                *accumulator = 0.0;
            }
            *accumulator += delta_y;
            let steps = (*accumulator / TERMINAL_ZOOM_PIXEL_STEP).trunc() as i32;
            *accumulator -= steps as f32 * TERMINAL_ZOOM_PIXEL_STEP;
            steps
        }
    }
}

impl Ashell {
    pub(crate) fn on_terminal_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cmd_ctrl_pressed = event.keystroke.modifiers.platform;
        // If the search input is focused, skip terminal key processing
        // so the input can handle text entry, paste, etc. normally.
        if self
            .search_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
        {
            return;
        }

        // Pane navigation: Alt + h/j/k/l
        if event.keystroke.modifiers.alt
            && !event.keystroke.modifiers.shift
            && !event.keystroke.modifiers.control
            && !event.keystroke.modifiers.platform
        {
            match event.keystroke.key.to_ascii_lowercase().as_str() {
                "h" => self.focus_adjacent_pane("left", window, cx),
                "j" => self.focus_adjacent_pane("down", window, cx),
                "k" => self.focus_adjacent_pane("up", window, cx),
                "l" => self.focus_adjacent_pane("right", window, cx),
                "q" => {
                    if let Some(active_id) = self.active_tab.clone() {
                        self.close_tab(active_id, cx);
                    }
                }
                _ => return,
            }
            window.prevent_default();
            cx.stop_propagation();
            cx.notify();
            return;
        }

        // Pane split: Shift+Alt + h/j/k/l
        if event.keystroke.modifiers.shift
            && event.keystroke.modifiers.alt
            && !event.keystroke.modifiers.control
            && !event.keystroke.modifiers.platform
        {
            let direction = match event.keystroke.key.to_ascii_lowercase().as_str() {
                "h" => Some("left"),
                "j" => Some("down"),
                "k" => Some("up"),
                "l" => Some("right"),
                _ => None,
            };
            if let Some(dir) = direction {
                self.split_current_pane(dir, cx);
                window.prevent_default();
                cx.stop_propagation();
                cx.notify();
                return;
            }
        }

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

        // If the active tab is disconnected and user presses Enter, reconnect
        if event.keystroke.key == "enter"
            && !event.keystroke.modifiers.shift
            && !event.keystroke.modifiers.control
            && !event.keystroke.modifiers.alt
            && !event.keystroke.modifiers.platform
        {
            if let Some(progress) = &self.connection_progress {
                if progress.failed {
                    self.retry_connection_progress(cx);
                    window.prevent_default();
                    cx.stop_propagation();
                    return;
                }
            }

            let active_id = self.active_tab.clone();
            if let Some(active_id) = active_id {
                let is_disconnected = self
                    .tabs
                    .iter()
                    .find(|t| t.id == active_id)
                    .is_some_and(|tab| tab.disconnected_reason.is_some());
                if is_disconnected {
                    self.retry_disconnected_tab(&active_id, cx);
                    window.prevent_default();
                    cx.stop_propagation();
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
        let Some(app_cursor_mode) = self
            .tabs
            .iter()
            .find(|tab| tab.id == active_id)
            .map(|tab| tab.app_cursor_mode())
        else {
            return;
        };

        let Some(bytes) = encode_key(&event.keystroke, app_cursor_mode, false) else {
            return;
        };
        self.send_terminal_input(bytes, window, cx);
    }

    pub(crate) fn on_terminal_tab_action(
        &mut self,
        _: &TerminalTabKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.send_terminal_input(vec![b'\t'], window, cx);
    }

    pub(crate) fn on_terminal_backtab_action(
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
        self.record_ssh_input(&active_id, &bytes);
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == active_id) else {
            return;
        };

        if tab.render_snapshot(false).display_offset > 0 {
            tab.scroll_to_bottom();
        }

        tab.clear_selection();
        tab.prepare_for_terminal_input();
        let encoded = tab.encode_input(&bytes);
        tab.send_backend(BackendCommand::Input(encoded));
        window.prevent_default();
        cx.stop_propagation();
        cx.notify();
    }

    pub(crate) fn execute_ssh_history_command(
        &mut self,
        command: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_connected_ssh = self
            .active_tab
            .as_ref()
            .and_then(|active_id| self.tabs.iter().find(|tab| &tab.id == active_id))
            .is_some_and(|tab| tab.kind == crate::terminal::TabKind::Ssh && tab.connected);
        if !is_connected_ssh {
            return;
        }
        let mut bytes = command.into_bytes();
        bytes.push(b'\r');
        self.send_terminal_input(bytes, window, cx);
        self.close_command_history(cx);
    }

    pub(crate) fn active_terminal_selection_text(&self) -> Option<String> {
        let active_id = self.active_tab.as_ref()?;
        self.tabs
            .iter()
            .find(|tab| &tab.id == active_id)
            .and_then(|tab| tab.selection_text())
    }

    pub(crate) fn paste_into_terminal(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(active_id) = self.active_tab.clone() else {
            return;
        };
        let normalized_text = text
            .replace('\x1b', "")
            .replace("\r\n", "\r")
            .replace('\n', "\r");
        self.record_ssh_input(&active_id, normalized_text.as_bytes());
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == active_id) else {
            return;
        };

        if tab.render_snapshot(false).display_offset > 0 {
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
        let bytes = text.as_bytes().to_vec();
        self.record_ssh_input(&active_id, &bytes);
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == active_id) else {
            return;
        };

        if tab.render_snapshot(false).display_offset > 0 {
            tab.scroll_to_bottom();
        }
        tab.clear_selection();
        self.terminal_marked_text = None;
        tab.prepare_for_terminal_input();
        let encoded = tab.encode_input(&bytes);
        tab.send_backend(BackendCommand::Input(encoded));
        window.invalidate_character_coordinates();
        cx.notify();
    }

    /// Track the current SSH shell line and persist completed commands.
    fn record_ssh_input(&mut self, tab_id: &str, bytes: &[u8]) {
        let (session_id, cursor, is_alternate_screen_active) = {
            let Some(tab) = self.tabs.iter().find(|tab| {
                tab.id == tab_id && tab.kind == crate::terminal::TabKind::Ssh && tab.connected
            }) else {
                return;
            };
            let Some(session) = tab.session.as_ref() else {
                return;
            };
            (
                session.id.clone(),
                tab.buffer_cursor_position(),
                tab.is_alternate_screen_active(),
            )
        };

        if is_alternate_screen_active {
            self.ssh_command_buffers.remove(tab_id);
            self.ssh_command_starts.remove(tab_id);
            self.ssh_command_input_uncertain.remove(tab_id);
            return;
        }

        let submits_command = bytes.iter().any(|byte| matches!(*byte, b'\r' | b'\n'));
        let edits_command = bytes
            .iter()
            .any(|byte| !matches!(*byte, b'\r' | b'\n' | b'\x03'));
        let mut input_uncertain = self.ssh_command_input_uncertain.contains(tab_id);
        if edits_command && !self.ssh_command_starts.contains_key(tab_id) {
            if let Some(cursor) = cursor {
                self.ssh_command_starts.insert(tab_id.to_string(), cursor);
            }
        }

        let mut rendered_command = if submits_command {
            self.ssh_command_starts
                .get(tab_id)
                .copied()
                .and_then(|start| {
                    self.tabs
                        .iter()
                        .find(|tab| tab.id == tab_id)
                        .map(|tab| tab.render_snapshot(false))
                        .and_then(|snapshot| terminal_command_text(&snapshot, start, cursor))
                })
        } else {
            None
        };

        let mut completed = Vec::new();
        let mut reset_command_start = false;
        {
            let buffer = self
                .ssh_command_buffers
                .entry(tab_id.to_string())
                .or_default();
            let mut in_escape = false;
            let mut in_csi = false;
            for character in String::from_utf8_lossy(bytes).chars() {
                if in_escape {
                    if character == '[' || character == 'O' {
                        in_escape = false;
                        in_csi = true;
                    } else {
                        in_escape = false;
                    }
                    continue;
                }
                if in_csi {
                    if character.is_ascii_alphabetic() || character == '~' {
                        in_csi = false;
                    }
                    continue;
                }
                match character {
                    '\x1b' => {
                        input_uncertain = true;
                        in_escape = true;
                    }
                    '\r' | '\n' => {
                        let command = command_history_text(
                            rendered_command.take().as_deref(),
                            buffer,
                            input_uncertain,
                        );
                        if !command.is_empty() {
                            completed.push(command);
                        }
                        buffer.clear();
                        reset_command_start = true;
                        input_uncertain = false;
                    }
                    '\u{8}' | '\u{7f}' => {
                        buffer.pop();
                    }
                    '\u{15}' => buffer.clear(),
                    '\u{3}' => {
                        buffer.clear();
                        reset_command_start = true;
                        input_uncertain = false;
                    }
                    '\u{17}' => {
                        let trimmed_len = buffer.trim_end().len();
                        buffer.truncate(trimmed_len);
                        while let Some((index, character)) = buffer.char_indices().next_back() {
                            if character.is_whitespace() {
                                break;
                            }
                            buffer.truncate(index);
                        }
                    }
                    character if !character.is_control() => buffer.push(character),
                    _ => input_uncertain = true,
                }
            }
        }
        if reset_command_start {
            self.ssh_command_starts.remove(tab_id);
            self.ssh_command_input_uncertain.remove(tab_id);
        } else if input_uncertain {
            self.ssh_command_input_uncertain.insert(tab_id.to_string());
        } else {
            self.ssh_command_input_uncertain.remove(tab_id);
        }

        let mut changed = false;
        for command in completed {
            changed |= self.config.add_command_history(&session_id, command);
        }
        if changed {
            self.selected_command_history.clear();
            self.save_preferences_background();
        }
    }

    pub(crate) fn on_terminal_right_click(
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

    pub(crate) fn begin_terminal_selection(
        &mut self,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
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

    pub(crate) fn move_terminal_cursor_to_click(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((target_row, target_col, _)) = self.terminal_grid_point_and_side(position) else {
            return false;
        };
        let Some(active_id) = self.active_tab.clone() else {
            return false;
        };
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == active_id) else {
            return false;
        };
        let mode = *tab.term.mode();
        let click_state = if tab.is_alternate_screen_active() || tab.mouse_tracking_enabled() {
            None
        } else {
            tab.prompt_input_click_state((target_row, target_col))
        };
        let (bytes, predicted_cursor) = if let Some(click_state) = click_state {
            if !prompt_click_is_valid((target_row, target_col), click_state.command_start) {
                return false;
            }
            match click_state.mode {
                crate::terminal::PromptClickMode::Absolute => (
                    sgr_prompt_click(
                        (target_row, target_col),
                        click_state.prompt_row_offset,
                        click_state.mode,
                    ),
                    None,
                ),
                crate::terminal::PromptClickMode::Relative if click_state.relative_click_valid => (
                    sgr_prompt_click(
                        (target_row, target_col),
                        click_state.prompt_row_offset,
                        click_state.mode,
                    ),
                    None,
                ),
                crate::terminal::PromptClickMode::Relative
                | crate::terminal::PromptClickMode::TerminalManaged => {
                    let Some(cursor) = tab.cursor_state_for_click() else {
                        return false;
                    };
                    let snapshot = tab.render_snapshot(false);
                    let movement = prompt_cursor_move(
                        &snapshot,
                        cursor,
                        (target_row, target_col),
                        &click_state.command_starts,
                        tab.app_cursor_mode(),
                    );
                    let predicted_cursor = crate::terminal::CursorState {
                        row: movement.target.0,
                        col: movement.target.1,
                        shape: cursor.shape,
                    };
                    (movement.bytes, Some(predicted_cursor))
                }
            }
        } else if tab.mouse_tracking_enabled() {
            (terminal_mouse_click((target_row, target_col), mode), None)
        } else if tab.is_alternate_screen_active() {
            let Some(cursor) = tab.cursor_state_for_click() else {
                return false;
            };
            let snapshot = tab.render_snapshot(false);
            let movement = alternate_screen_cursor_move(
                &snapshot,
                cursor,
                (target_row, target_col),
                tab.app_cursor_mode(),
            );
            let predicted_cursor = crate::terminal::CursorState {
                row: movement.target.0,
                col: movement.target.1,
                shape: cursor.shape,
            };
            (movement.bytes, Some(predicted_cursor))
        } else {
            let Some(cursor) = tab.cursor_state_for_click() else {
                return false;
            };
            let snapshot = tab.render_snapshot(false);
            let movement = prompt_cursor_move(
                &snapshot,
                cursor,
                (target_row, target_col),
                &[],
                tab.app_cursor_mode(),
            );
            let predicted_cursor = crate::terminal::CursorState {
                row: movement.target.0,
                col: movement.target.1,
                shape: cursor.shape,
            };
            (movement.bytes, Some(predicted_cursor))
        };
        tab.clear_selection();
        if let Some(predicted_cursor) = predicted_cursor {
            if !bytes.is_empty() {
                tab.note_click_cursor_move(predicted_cursor);
            }
        } else {
            tab.clear_click_cursor_prediction();
        }
        if !bytes.is_empty() {
            tab.send_backend(crate::terminal::BackendCommand::Input(bytes));
        }
        window.prevent_default();
        cx.stop_propagation();
        cx.notify();
        true
    }

    pub(crate) fn on_terminal_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Handle split drag
        if self.dragging_splitter.is_some() {
            if event.pressed_button == Some(MouseButton::Left) {
                self.on_split_drag_move(event, window);
                cx.notify();
            } else {
                self.end_drag_split();
                cx.notify();
            }
            return;
        }

        // Track URL hover
        let mut hovered_url = None;
        let cmd_ctrl_pressed = event.modifiers.platform;
        if let Some((row, col, _side)) = self.terminal_grid_point_and_side(event.position) {
            if let Some(snapshot) = self.active_snapshot() {
                if let Some(active_id) = &self.active_tab {
                    if let Some((url, url_cells)) = crate::terminal::highlight::find_url_at_cell(
                        &snapshot.cells,
                        snapshot.rows,
                        row,
                        col,
                    ) {
                        hovered_url = Some(crate::app::HoveredUrl {
                            url,
                            tab_id: active_id.clone(),
                            cells: url_cells,
                        });
                    }
                }
            }
        }

        if self.hovered_url != hovered_url || self.cmd_ctrl_pressed != cmd_ctrl_pressed {
            self.hovered_url = hovered_url;
            self.cmd_ctrl_pressed = cmd_ctrl_pressed;
            cx.notify();
        }

        if !self.terminal_selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let Some((row, col, side)) = self.terminal_grid_point_and_side(event.position) else {
            return;
        };
        let Some(active_id) = self.active_tab.clone() else {
            return;
        };
        let snapshot = match self.active_snapshot() {
            Some(s) => s,
            None => return,
        };
        let max_row = snapshot.rows.saturating_sub(1);

        let mut scroll_delta = 0i32;
        if max_row >= 6 {
            if row <= 2 || row >= max_row.saturating_sub(2) {
                let now = std::time::Instant::now();
                let should_scroll = LAST_DRAG_SCROLL.with(|last| {
                    if let Some(last_time) = last.get() {
                        if now.duration_since(last_time) >= std::time::Duration::from_millis(80) {
                            last.set(Some(now));
                            true
                        } else {
                            false
                        }
                    } else {
                        last.set(Some(now));
                        true
                    }
                });

                if should_scroll {
                    if row == 0 {
                        scroll_delta = 2;
                    } else if row == 1 || row == 2 {
                        scroll_delta = 1;
                    } else if row == max_row {
                        scroll_delta = -2;
                    } else if row == max_row.saturating_sub(1) || row == max_row.saturating_sub(2) {
                        scroll_delta = -1;
                    }
                }
            } else {
                LAST_DRAG_SCROLL.with(|last| last.set(None));
            }
        }

        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == active_id) {
            if scroll_delta != 0 {
                tab.scroll_history(scroll_delta);
            }
            tab.update_selection(row, col, side);
            cx.notify();
        }
    }

    pub(crate) fn on_terminal_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dragging_splitter.is_some() {
            self.end_drag_split();
        }
        self.terminal_selecting = false;
        cx.notify();
    }

    pub(crate) fn terminal_grid_point_and_side(
        &self,
        position: Point<Pixels>,
    ) -> Option<(usize, usize, Side)> {
        let active_id = self.active_tab.as_ref()?;
        let bounds = self.terminal_bounds.get(active_id)?;
        if !bounds.contains(&position) {
            // Try other pane bounds
            for b in self.terminal_bounds.values() {
                if b.contains(&position) {
                    // Found a different pane - focus it
                    // (this path is for click-to-focus; handled via focus_terminal)
                    return None;
                }
            }
            return None;
        }
        let local_x = (position.x - bounds.origin.x).max(px(0.));
        let local_y = (position.y - bounds.origin.y).max(px(0.));
        let cell_width = px(self.terminal_cell_width());
        let line_height = px(self.terminal_line_height());
        let snapshot = self.active_snapshot()?;
        let max_col = snapshot.cols.saturating_sub(1);
        let col = ((local_x / cell_width).floor() as usize).min(max_col);
        let row = terminal_grid_row(
            local_y.as_f32(),
            bounds.size.height.as_f32(),
            line_height.as_f32(),
            snapshot.rows,
        );
        let cell_offset_x = px(local_x.as_f32() % cell_width.as_f32());
        let side = if cell_offset_x >= (cell_width / 2.) {
            Side::Right
        } else {
            Side::Left
        };
        Some((row, col, side))
    }

    pub(crate) fn on_terminal_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Platform modifier (Cmd on macOS, Ctrl on Windows/Linux) + scroll → zoom terminal font size
        if event.modifiers.platform {
            let zoom_steps = terminal_zoom_steps(&event.delta, &mut self.terminal_zoom_accumulator);
            if zoom_steps != 0 {
                self.change_terminal_font_size(zoom_steps, window, cx);
            }
            window.prevent_default();
            cx.stop_propagation();
            return;
        }

        let Some(active_id) = self.active_tab.clone() else {
            return;
        };

        // Get coordinates before mutably borrowing tabs
        let grid_point = self.terminal_grid_point_and_side(event.position);

        let line_height = self.terminal_line_height();

        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == active_id) {
            let delta_lines = match event.delta {
                ScrollDelta::Lines(point) => point.y.round() as i32,
                ScrollDelta::Pixels(point) => {
                    tab.scroll_pixel_y += point.y.as_f32();
                    let lines = (tab.scroll_pixel_y / line_height).trunc() as i32;
                    tab.scroll_pixel_y -= (lines as f32) * line_height;
                    lines
                }
            };

            if delta_lines == 0 {
                return;
            }

            tab.clear_click_cursor_prediction();
            let mode = tab.term.mode();

            let is_mouse_tracking = mode.intersects(
                alacritty_terminal::term::TermMode::MOUSE_REPORT_CLICK
                    | alacritty_terminal::term::TermMode::MOUSE_MOTION
                    | alacritty_terminal::term::TermMode::MOUSE_DRAG,
            );

            let is_alternate_scroll = mode.contains(
                alacritty_terminal::term::TermMode::ALT_SCREEN
                    | alacritty_terminal::term::TermMode::ALTERNATE_SCROLL,
            );

            if is_mouse_tracking {
                if let Some((row, col, _)) = grid_point {
                    let sgr = mode.contains(alacritty_terminal::term::TermMode::SGR_MOUSE);
                    let button = if delta_lines > 0 { 64 } else { 65 };
                    let times = delta_lines.abs();
                    let mut bytes = Vec::new();
                    for _ in 0..times {
                        if sgr {
                            bytes.extend_from_slice(
                                format!("\x1b[<{};{};{}M", button, col + 1, row + 1).as_bytes(),
                            );
                        } else {
                            if col < 223 && row < 223 {
                                bytes.extend_from_slice(b"\x1b[M");
                                bytes.push(button as u8 + 32);
                                bytes.push(col as u8 + 33);
                                bytes.push(row as u8 + 33);
                            }
                        }
                    }
                    if !bytes.is_empty() {
                        tab.send_backend(crate::terminal::BackendCommand::Input(bytes));
                    }
                }
                window.prevent_default();
                cx.stop_propagation();
                return;
            } else if is_alternate_scroll {
                let times = delta_lines.abs();
                let code = if delta_lines > 0 { b'A' } else { b'B' };
                let mut bytes = Vec::with_capacity((times * 3) as usize);
                for _ in 0..times {
                    bytes.extend_from_slice(&[b'\x1b', b'O', code]);
                }
                if !bytes.is_empty() {
                    tab.send_backend(crate::terminal::BackendCommand::Input(bytes));
                }
                window.prevent_default();
                cx.stop_propagation();
                return;
            }

            tab.scroll_history(delta_lines);
            window.prevent_default();
            cx.stop_propagation();
            cx.notify();
        }
    }
}

fn terminal_grid_row(
    local_y: f32,
    container_height: f32,
    line_height: f32,
    row_count: usize,
) -> usize {
    let grid_height = (container_height / line_height).floor().max(1.0) * line_height;
    let y_offset = ((container_height - grid_height) / 2.0).max(0.0);
    (((local_y - y_offset).max(0.0) / line_height).floor() as usize)
        .min(row_count.saturating_sub(1))
}

fn prompt_click_is_valid(target: (usize, usize), command_start: (usize, usize)) -> bool {
    target.0 > command_start.0 || (target.0 == command_start.0 && target.1 >= command_start.1)
}

fn append_cursor_key(bytes: &mut Vec<u8>, key: u8, app_cursor_mode: bool) {
    bytes.extend_from_slice(&crate::terminal::encode_cursor_key(key, app_cursor_mode));
}

fn snapshot_cell_widths(snapshot: &crate::terminal::RenderSnapshot) -> Vec<usize> {
    let mut cell_widths = vec![1usize; snapshot.rows.saturating_mul(snapshot.cols)];
    for render_cell in &snapshot.cells {
        let Ok(row) = usize::try_from(render_cell.row) else {
            continue;
        };
        let Ok(col) = usize::try_from(render_cell.col) else {
            continue;
        };
        if row >= snapshot.rows || col >= snapshot.cols {
            continue;
        }

        let flags = render_cell.cell.flags;
        cell_widths[row * snapshot.cols + col] =
            if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                0
            } else if flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };
    }
    cell_widths
}

fn snap_cursor_col(cell_widths: &[usize], row: usize, col: usize, cols: usize) -> usize {
    let mut col = col.min(cols.saturating_sub(1));
    while col > 0 && cell_widths.get(row * cols + col).copied() == Some(0) {
        col -= 1;
    }
    col
}

fn horizontal_cursor_move_count(
    cell_widths: &[usize],
    row: usize,
    source_col: usize,
    target_col: usize,
    cols: usize,
) -> usize {
    if source_col == target_col || cols == 0 {
        return 0;
    }

    let mut count = 0;
    let mut col = source_col.min(cols - 1);
    if col < target_col {
        while col < target_col {
            col += 1;
            while col < target_col && cell_widths.get(row * cols + col).copied() == Some(0) {
                col += 1;
            }
            count += 1;
        }
    } else {
        while col > target_col {
            col -= 1;
            while col > target_col && cell_widths.get(row * cols + col).copied() == Some(0) {
                col -= 1;
            }
            count += 1;
        }
    }
    count
}

fn append_horizontal_cursor_move(
    bytes: &mut Vec<u8>,
    cell_widths: &[usize],
    row: usize,
    source_col: usize,
    target_col: usize,
    cols: usize,
    app_cursor_mode: bool,
) {
    let key = if target_col < source_col { b'D' } else { b'C' };
    let count = horizontal_cursor_move_count(cell_widths, row, source_col, target_col, cols);
    for _ in 0..count {
        append_cursor_key(bytes, key, app_cursor_mode);
    }
}

#[derive(Debug, PartialEq, Eq)]
struct CursorMove {
    bytes: Vec<u8>,
    target: (usize, usize),
}

fn alternate_screen_cursor_move(
    snapshot: &crate::terminal::RenderSnapshot,
    cursor: crate::terminal::CursorState,
    target: (usize, usize),
    app_cursor_mode: bool,
) -> CursorMove {
    // Mouse-aware apps bypass this helper and receive exact cell coordinates.
    // Otherwise mirror iTerm2's predictive fallback; rendered cells no longer
    // retain enough information to distinguish tabs from literal spaces.
    if snapshot.rows == 0 || snapshot.cols == 0 {
        return CursorMove {
            bytes: Vec::new(),
            target: (cursor.row, cursor.col),
        };
    }

    let cell_widths = snapshot_cell_widths(snapshot);
    let cursor_row = cursor.row.min(snapshot.rows - 1);
    let mut position = (
        cursor_row,
        snap_cursor_col(&cell_widths, cursor_row, cursor.col, snapshot.cols),
    );
    let target_row = target.0.min(snapshot.rows - 1);
    let target = (
        target_row,
        snap_cursor_col(&cell_widths, target_row, target.1, snapshot.cols),
    );
    if position == target {
        return CursorMove {
            bytes: Vec::new(),
            target,
        };
    }

    let estimated_moves = position.0.abs_diff(target.0) + position.1.abs_diff(target.1);
    let mut bytes = Vec::with_capacity(estimated_moves.saturating_mul(3));

    // Match iTerm2's ordering so vertical movement cannot clamp a cursor that
    // first needs to move left to reach the requested column.
    if position.1 > target.1 {
        let pre_vertical_col = snap_cursor_col(&cell_widths, position.0, target.1, snapshot.cols);
        append_horizontal_cursor_move(
            &mut bytes,
            &cell_widths,
            position.0,
            position.1,
            pre_vertical_col,
            snapshot.cols,
            app_cursor_mode,
        );
        position.1 = pre_vertical_col;
    }

    let vertical_key = if target.0 < position.0 { b'A' } else { b'B' };
    for _ in 0..position.0.abs_diff(target.0) {
        append_cursor_key(&mut bytes, vertical_key, app_cursor_mode);
    }
    position.0 = target.0;
    position.1 = snap_cursor_col(&cell_widths, position.0, position.1, snapshot.cols);

    if position.1 != target.1 {
        append_horizontal_cursor_move(
            &mut bytes,
            &cell_widths,
            position.0,
            position.1,
            target.1,
            snapshot.cols,
            app_cursor_mode,
        );
    }

    CursorMove { bytes, target }
}

fn prompt_cursor_move(
    snapshot: &crate::terminal::RenderSnapshot,
    cursor: crate::terminal::CursorState,
    target: (usize, usize),
    command_starts: &[(usize, usize)],
    app_cursor_mode: bool,
) -> CursorMove {
    if snapshot.rows == 0 || snapshot.cols == 0 {
        return CursorMove {
            bytes: Vec::new(),
            target: (cursor.row, cursor.col),
        };
    }

    let cell_widths = snapshot_cell_widths(snapshot);
    let target_row = target.0.min(snapshot.rows.saturating_sub(1));
    let target = (
        target_row,
        snap_cursor_col(&cell_widths, target_row, target.1, snapshot.cols),
    );
    let cursor_row = cursor.row.min(snapshot.rows.saturating_sub(1));
    let cursor_point = (
        cursor_row,
        snap_cursor_col(&cell_widths, cursor_row, cursor.col, snapshot.cols),
    );
    if target == cursor_point {
        return CursorMove {
            bytes: Vec::new(),
            target,
        };
    }

    let (start, end, key) = if target < cursor_point {
        (target, cursor_point, b'D')
    } else {
        (cursor_point, target, b'C')
    };
    let mut row_text_starts: Vec<Option<usize>> = vec![None; snapshot.rows];
    let mut row_text_ends: Vec<Option<usize>> = vec![None; snapshot.rows];
    let mut row_wraps = vec![false; snapshot.rows];
    for render_cell in &snapshot.cells {
        let Ok(row) = usize::try_from(render_cell.row) else {
            continue;
        };
        let Ok(col) = usize::try_from(render_cell.col) else {
            continue;
        };
        if row >= snapshot.rows || col >= snapshot.cols {
            continue;
        }
        let flags = render_cell.cell.flags;
        row_wraps[row] |= flags.contains(Flags::WRAPLINE);
        if !flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            let width = cell_widths[row * snapshot.cols + col];
            if render_cell.cell.c != ' '
                || render_cell
                    .cell
                    .zerowidth()
                    .is_some_and(|characters| !characters.is_empty())
            {
                row_text_starts[row] =
                    Some(row_text_starts[row].map_or(col, |start| start.min(col)));
                row_text_ends[row] = Some(
                    row_text_ends[row]
                        .map_or(col.saturating_add(width), |end| {
                            end.max(col.saturating_add(width))
                        })
                        .min(snapshot.cols),
                );
            }
        }
    }

    // Spaces between two cursor positions are real input positions. Only trim
    // the unused margin of an explicitly-broken row; wrapped rows consume the
    // complete terminal width.
    let mut count = 0;
    for row in start.0..=end.0 {
        let mut col = if row == start.0 {
            start.1
        } else {
            command_starts
                .iter()
                .rev()
                .find(|(command_row, _)| *command_row == row)
                .map(|(_, command_col)| *command_col)
                .or_else(|| {
                    if row > 0 && row_wraps[row - 1] {
                        Some(0)
                    } else {
                        row_text_starts[row]
                    }
                })
                .unwrap_or(0)
        };
        let col_limit = if row == end.0 {
            end.1.min(snapshot.cols)
        } else if row_wraps[row] {
            snapshot.cols
        } else {
            row_text_ends[row].unwrap_or(col)
        };
        while col < col_limit {
            let width = cell_widths[row * snapshot.cols + col];
            if width == 0 {
                col += 1;
                continue;
            }
            count += 1;
            col = col.saturating_add(width);
        }
        if row < end.0 && !row_wraps[row] {
            count += 1;
        }
    }

    let mut bytes = Vec::with_capacity(count * 3);
    for _ in 0..count {
        append_cursor_key(&mut bytes, key, app_cursor_mode);
    }
    CursorMove { bytes, target }
}

fn sgr_prompt_click(
    target: (usize, usize),
    prompt_row_offset: usize,
    click_mode: crate::terminal::PromptClickMode,
) -> Vec<u8> {
    let row = match click_mode {
        crate::terminal::PromptClickMode::Absolute
        | crate::terminal::PromptClickMode::TerminalManaged => target.0 + 1,
        crate::terminal::PromptClickMode::Relative => prompt_row_offset + 1,
    };
    format!("\x1b[<0;{};{}M", target.1 + 1, row).into_bytes()
}

fn terminal_mouse_click(target: (usize, usize), mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::SGR_MOUSE) {
        return format!(
            "\x1b[<0;{};{}M\x1b[<0;{};{}m",
            target.1 + 1,
            target.0 + 1,
            target.1 + 1,
            target.0 + 1
        )
        .into_bytes();
    }

    let utf8 = mode.contains(TermMode::UTF8_MOUSE);
    let max_point = if utf8 { 2015 } else { 223 };
    if target.0 >= max_point || target.1 >= max_point {
        return Vec::new();
    }
    let mut bytes = Vec::with_capacity(12);
    for button in [0u8, 3u8] {
        bytes.extend_from_slice(b"\x1b[M");
        bytes.push(32 + button);
        append_mouse_coordinate(&mut bytes, target.1, utf8);
        append_mouse_coordinate(&mut bytes, target.0, utf8);
    }
    bytes
}

fn append_mouse_coordinate(bytes: &mut Vec<u8>, position: usize, utf8: bool) {
    let encoded = position + 33;
    if utf8 && position >= 95 {
        let mut buffer = [0; 4];
        bytes.extend_from_slice(
            char::from_u32(encoded as u32)
                .expect("mouse coordinate is a valid Unicode scalar")
                .encode_utf8(&mut buffer)
                .as_bytes(),
        );
    } else {
        bytes.push(encoded as u8);
    }
}

fn terminal_command_text(
    snapshot: &crate::terminal::RenderSnapshot,
    start: (usize, usize),
    end: Option<(usize, usize)>,
) -> Option<String> {
    let start = buffer_position_in_viewport(snapshot, start)?;
    let end = match end {
        Some(position) => Some(buffer_position_in_viewport(snapshot, position)?),
        None => None,
    };
    let logical_lines =
        crate::terminal::highlight::build_logical_lines(&snapshot.cells, snapshot.rows);
    for line in logical_lines {
        if !line.byte_to_cell.iter().any(|(row, _)| *row == start.0) {
            continue;
        }

        let start_byte = line
            .byte_to_cell
            .iter()
            .position(|(row, col)| *row > start.0 || (*row == start.0 && *col >= start.1))?;
        let end_byte = end
            .filter(|(row, col)| *row > start.0 || (*row == start.0 && *col >= start.1))
            .and_then(|(row, col)| {
                line.byte_to_cell.iter().position(|(line_row, line_col)| {
                    *line_row > row || (*line_row == row && *line_col >= col)
                })
            })
            .unwrap_or(line.text.len());
        let command = line
            .text
            .get(start_byte..end_byte)?
            .trim_end_matches(|character: char| character == '\0' || character.is_whitespace())
            .replace('\0', "");
        if !command.trim().is_empty() {
            return Some(command);
        }
    }
    None
}

fn buffer_position_in_viewport(
    snapshot: &crate::terminal::RenderSnapshot,
    position: (usize, usize),
) -> Option<(usize, usize)> {
    let viewport_start = snapshot
        .history_size
        .saturating_sub(snapshot.display_offset);
    let row = position.0.checked_sub(viewport_start)?;
    (row < snapshot.rows && position.1 < snapshot.cols).then_some((row, position.1))
}

fn command_history_text(rendered: Option<&str>, buffered: &str, input_uncertain: bool) -> String {
    let rendered = rendered.unwrap_or_default().trim();
    let buffered = buffered.trim();
    if !input_uncertain && !buffered.is_empty() {
        return buffered.to_string();
    }
    if rendered.is_empty() {
        return buffered.to_string();
    }
    if buffered.is_empty() {
        return rendered.to_string();
    }
    // Completion extends the current token; a new argument after exact raw input is stale content.
    if rendered
        .strip_prefix(buffered)
        .and_then(|suffix| suffix.chars().next())
        .is_some_and(char::is_whitespace)
    {
        return buffered.to_string();
    }
    rendered.to_string()
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::term::{
        TermMode,
        cell::{Cell, Flags},
    };
    use alacritty_terminal::vte::ansi::CursorShape;
    use gpui::{ScrollDelta, point, px};

    use super::{
        alternate_screen_cursor_move, command_history_text, prompt_click_is_valid,
        prompt_cursor_move, sgr_prompt_click, terminal_command_text, terminal_grid_row,
        terminal_mouse_click, terminal_zoom_steps,
    };
    use crate::terminal::{CursorState, PromptClickMode, RenderCell, RenderSnapshot};

    fn snapshot(rows: &[&str], cols: usize) -> RenderSnapshot {
        let mut cells = Vec::with_capacity(rows.len() * cols);
        for (row, text) in rows.iter().enumerate() {
            let characters = text.chars().collect::<Vec<_>>();
            for col in 0..cols {
                let mut cell = Cell::default();
                cell.c = characters.get(col).copied().unwrap_or(' ');
                cells.push(RenderCell {
                    row: row as i32,
                    col: col as i32,
                    cell,
                });
            }
        }
        RenderSnapshot {
            cells,
            cursor: None,
            selection: None,
            display_offset: 0,
            history_size: 0,
            rows: rows.len(),
            cols,
            highlights: Default::default(),
        }
    }

    #[test]
    fn terminal_zoom_uses_whole_pixel_steps_for_line_scrolls() {
        let mut accumulator = 10.0;

        assert_eq!(
            terminal_zoom_steps(&ScrollDelta::Lines(point(0.0, 4.0)), &mut accumulator),
            1
        );
        assert_eq!(accumulator, 0.0);
        assert_eq!(
            terminal_zoom_steps(&ScrollDelta::Lines(point(0.0, -3.0)), &mut accumulator),
            -1
        );
    }

    #[test]
    fn terminal_zoom_accumulates_precise_scrolls_into_whole_pixel_steps() {
        let mut accumulator = 0.0;

        assert_eq!(
            terminal_zoom_steps(
                &ScrollDelta::Pixels(point(px(0.0), px(10.0))),
                &mut accumulator,
            ),
            0
        );
        assert_eq!(accumulator, 10.0);
        assert_eq!(
            terminal_zoom_steps(
                &ScrollDelta::Pixels(point(px(0.0), px(10.0))),
                &mut accumulator,
            ),
            1
        );
        assert_eq!(accumulator, 0.0);
    }

    #[test]
    fn accounts_for_vertical_grid_centering_when_mapping_clicks() {
        assert_eq!(terminal_grid_row(10.25, 41.0, 10.0, 4), 0);
        assert_eq!(terminal_grid_row(10.75, 41.0, 10.0, 4), 1);
    }

    #[test]
    fn limits_cursor_clicks_to_the_current_prompt_input() {
        assert!(!prompt_click_is_valid((2, 8), (3, 5)));
        assert!(!prompt_click_is_valid((3, 4), (3, 5)));
        assert!(prompt_click_is_valid((3, 5), (3, 5)));
        assert!(prompt_click_is_valid((4, 0), (3, 5)));
        assert!(prompt_click_is_valid((5, 0), (3, 5)));
    }

    #[test]
    fn moves_across_wrapped_prompt_rows_with_horizontal_keys() {
        let mut snapshot = snapshot(&["$ abcdef", "ghijk   "], 8);
        snapshot
            .cells
            .iter_mut()
            .find(|cell| cell.row == 0 && cell.col == 7)
            .unwrap()
            .cell
            .flags
            .insert(Flags::WRAPLINE);
        let cursor = CursorState {
            row: 1,
            col: 5,
            shape: CursorShape::Block,
        };

        assert_eq!(
            prompt_cursor_move(&snapshot, cursor, (0, 4), &[(0, 2)], false).bytes,
            b"\x1b[D".repeat(9)
        );
    }

    #[test]
    fn counts_leading_spaces_on_a_wrapped_prompt_row() {
        let mut snapshot = snapshot(&["$ abcdef", "  ghijk "], 8);
        snapshot
            .cells
            .iter_mut()
            .find(|cell| cell.row == 0 && cell.col == 7)
            .unwrap()
            .cell
            .flags
            .insert(Flags::WRAPLINE);
        let cursor = CursorState {
            row: 1,
            col: 7,
            shape: CursorShape::Block,
        };

        assert_eq!(
            prompt_cursor_move(&snapshot, cursor, (0, 4), &[(0, 2)], false).bytes,
            b"\x1b[D".repeat(11)
        );
    }

    #[test]
    fn counts_spaces_between_prompt_cursor_positions() {
        let snapshot = snapshot(&["$ cargo run "], 12);
        let cursor = CursorState {
            row: 0,
            col: 11,
            shape: CursorShape::Block,
        };

        assert_eq!(
            prompt_cursor_move(&snapshot, cursor, (0, 4), &[(0, 2)], false).bytes,
            b"\x1b[D".repeat(7)
        );
    }

    #[test]
    fn skips_secondary_prompts_when_moving_across_explicit_lines() {
        let snapshot = snapshot(&["$ echo foo  ", "> bar       "], 12);
        let cursor = CursorState {
            row: 1,
            col: 5,
            shape: CursorShape::Block,
        };

        assert_eq!(
            prompt_cursor_move(&snapshot, cursor, (0, 4), &[(0, 2), (1, 2)], false,).bytes,
            b"\x1b[D".repeat(10)
        );
    }

    #[test]
    fn batches_vim_style_movement_in_iterm_order() {
        let snapshot = snapshot(&["abcdefghij", "abcdefghij", "abcdefghij"], 10);
        let cursor = CursorState {
            row: 2,
            col: 8,
            shape: CursorShape::Block,
        };

        assert_eq!(
            alternate_screen_cursor_move(&snapshot, cursor, (0, 3), false).bytes,
            [b"\x1b[D".repeat(5), b"\x1b[A".repeat(2)].concat()
        );

        let cursor = CursorState {
            row: 2,
            col: 3,
            shape: CursorShape::Block,
        };
        assert_eq!(
            alternate_screen_cursor_move(&snapshot, cursor, (0, 8), true).bytes,
            [b"\x1bOA".repeat(2), b"\x1bOC".repeat(5)].concat()
        );
    }

    #[test]
    fn snaps_vim_style_movement_off_wide_character_spacers() {
        let mut snapshot = snapshot(&["abW def"], 8);
        snapshot.cells[2].cell.flags.insert(Flags::WIDE_CHAR);
        snapshot.cells[3].cell.flags.insert(Flags::WIDE_CHAR_SPACER);
        let cursor = CursorState {
            row: 0,
            col: 6,
            shape: CursorShape::Block,
        };
        let movement = alternate_screen_cursor_move(&snapshot, cursor, (0, 3), false);

        assert_eq!(movement.bytes, b"\x1b[D".repeat(3));
        assert_eq!(movement.target, (0, 2));
    }

    #[test]
    fn corrects_column_after_crossing_a_wide_character_on_another_row() {
        let mut snapshot = snapshot(&["abcdefgh", "abcdefgh", "abW defg"], 8);
        snapshot.cells[18].cell.flags.insert(Flags::WIDE_CHAR);
        snapshot.cells[19]
            .cell
            .flags
            .insert(Flags::WIDE_CHAR_SPACER);
        let cursor = CursorState {
            row: 2,
            col: 6,
            shape: CursorShape::Block,
        };

        assert_eq!(
            alternate_screen_cursor_move(&snapshot, cursor, (0, 3), false).bytes,
            [b"\x1b[D".repeat(3), b"\x1b[A".repeat(2), b"\x1b[C".to_vec(),].concat()
        );
    }

    #[test]
    fn encodes_sgr_click_coordinates_for_prompt_modes() {
        assert_eq!(
            sgr_prompt_click((4, 7), 2, PromptClickMode::Absolute),
            b"\x1b[<0;8;5M".to_vec()
        );
        assert_eq!(
            sgr_prompt_click((4, 7), 2, PromptClickMode::Relative),
            b"\x1b[<0;8;3M".to_vec()
        );
    }

    #[test]
    fn encodes_plain_terminal_mouse_clicks_for_full_screen_apps() {
        assert_eq!(
            terminal_mouse_click((4, 7), TermMode::SGR_MOUSE),
            b"\x1b[<0;8;5M\x1b[<0;8;5m".to_vec()
        );
        assert_eq!(
            terminal_mouse_click((2, 3), TermMode::NONE),
            vec![0x1b, b'[', b'M', 32, 36, 35, 0x1b, b'[', b'M', 35, 36, 35]
        );
    }

    #[test]
    fn prefers_direct_input_over_stale_rendered_command_suffix() {
        let command = "sh /site/vocano/vocano-restart.sh";
        let rendered = format!("{command} /sivovo-re");

        assert_eq!(
            command_history_text(Some(rendered.as_str()), command, false),
            command
        );
    }

    #[test]
    fn rejects_whitespace_delimited_screen_suffix_when_input_is_uncertain() {
        let command = "sh /site/jimureport/jimureport-restart.sh";
        let rendered = format!("{command} /sijiji-r");

        assert_eq!(
            command_history_text(Some(rendered.as_str()), command, true),
            command
        );
    }

    #[test]
    fn does_not_append_tab_completion_keystroke_fragments() {
        let command = "sh /site/jimureport/jimureport-restart.sh";
        let buffered = "sh /sijiji-r";

        assert_eq!(command_history_text(Some(command), buffered, true), command);
    }

    #[test]
    fn falls_back_to_raw_input_when_uncertain_screen_text_is_unavailable() {
        let command = "sh /site/jimureport/jimureport-restart.sh";

        assert_eq!(command_history_text(None, command, true), command);
    }

    #[test]
    fn keeps_screen_completion_that_extends_the_current_argument() {
        let buffered = "sh /site/jimu";
        let rendered = "sh /site/jimureport/jimureport-restart.sh";

        assert_eq!(
            command_history_text(Some(rendered), buffered, true),
            rendered
        );
    }

    #[test]
    fn excludes_hidden_screen_residue_from_rendered_command() {
        let command = "sh /site/jimureport/jimureport-restart.sh";
        let rendered = format!("$ {command} /sijiji-r");
        let mut snapshot = snapshot(&[rendered.as_str()], rendered.len());
        for cell in snapshot.cells.iter_mut().skip(2 + command.chars().count()) {
            cell.cell.flags.insert(Flags::HIDDEN);
        }

        assert_eq!(
            terminal_command_text(&snapshot, (0, 2), None),
            Some(command.to_string())
        );
    }

    #[test]
    fn truncates_rendered_command_at_the_submission_cursor() {
        let command = "sh /site/vocano/vocano-restart.sh";
        let rendered = format!("$ {command} /sivovo-re");
        let snapshot = snapshot(&[rendered.as_str()], rendered.len());

        assert_eq!(
            terminal_command_text(&snapshot, (0, 2), Some((0, 2 + command.len())),),
            Some(command.to_string())
        );
    }

    #[test]
    fn keeps_command_coordinates_stable_when_wrapping_scrolls_the_viewport() {
        let first_row = "$ sh /site/jimureport/jimureport-";
        let second_row = "restart.sh /sijiji-r";
        let mut snapshot = snapshot(&[first_row, second_row], first_row.len());
        snapshot.history_size = 3;
        snapshot.display_offset = 2;
        snapshot
            .cells
            .iter_mut()
            .find(|cell| cell.row == 0 && cell.col == first_row.len() as i32 - 1)
            .unwrap()
            .cell
            .flags
            .insert(Flags::WRAPLINE);

        assert_eq!(
            terminal_command_text(&snapshot, (1, 2), Some((2, "restart.sh".len()))),
            Some("sh /site/jimureport/jimureport-restart.sh".to_string())
        );
    }
}

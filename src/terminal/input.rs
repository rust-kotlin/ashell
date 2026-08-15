use std::ops::Range;

use alacritty_terminal::index::Side;
use alacritty_terminal::selection::SelectionType;
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
        tab.record_terminal_input(&bytes);
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
                tab.cursor_state().map(|cursor| (cursor.row, cursor.col)),
                tab.is_alternate_screen_active(),
            )
        };

        if is_alternate_screen_active {
            self.ssh_command_buffers.remove(tab_id);
            self.ssh_command_starts.remove(tab_id);
            return;
        }

        let submits_command = bytes.iter().any(|byte| matches!(*byte, b'\r' | b'\n'));
        let edits_command = bytes
            .iter()
            .any(|byte| !matches!(*byte, b'\r' | b'\n' | b'\x03'));
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
                        .and_then(|snapshot| terminal_command_text(&snapshot, start))
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
                    '\x1b' => in_escape = true,
                    '\r' | '\n' => {
                        let command =
                            merge_command_text(rendered_command.take().as_deref(), buffer);
                        if !command.is_empty() {
                            completed.push(command);
                        }
                        buffer.clear();
                        reset_command_start = true;
                    }
                    '\u{8}' | '\u{7f}' => {
                        buffer.pop();
                    }
                    '\u{15}' => buffer.clear(),
                    '\u{3}' => {
                        buffer.clear();
                        reset_command_start = true;
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
                    _ => {}
                }
            }
        }
        if reset_command_start {
            self.ssh_command_starts.remove(tab_id);
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

    pub(crate) fn on_terminal_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Handle split drag
        if self.dragging_splitter.is_some() {
            if event.pressed_button == Some(MouseButton::Left) {
                self.on_split_drag_move(event, window, cx);
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

    pub(crate) fn on_terminal_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Platform modifier (Cmd on macOS, Ctrl on Windows/Linux) + scroll → zoom terminal font size
        if event.modifiers.platform {
            let delta = match event.delta {
                ScrollDelta::Lines(point) => point.y * 20.0,
                ScrollDelta::Pixels(point) => point.y.as_f32(),
            };
            self.terminal_zoom_accumulator += delta;
            let step = 20.0;
            if self.terminal_zoom_accumulator.abs() >= step {
                let zoom_steps = (self.terminal_zoom_accumulator / step).trunc();
                self.terminal_zoom_accumulator -= zoom_steps * step;
                self.change_terminal_font_size(zoom_steps * 0.5, cx);
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

fn terminal_command_text(
    snapshot: &crate::terminal::RenderSnapshot,
    start: (usize, usize),
) -> Option<String> {
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
        let command = line
            .text
            .get(start_byte..)?
            .trim_end_matches(|character: char| character == '\0' || character.is_whitespace())
            .replace('\0', "");
        if !command.trim().is_empty() {
            return Some(command);
        }
    }
    None
}

fn merge_command_text(rendered: Option<&str>, buffered: &str) -> String {
    let rendered = rendered.unwrap_or_default().trim();
    let buffered = buffered.trim();
    if rendered.is_empty() {
        return buffered.to_string();
    }
    if buffered.is_empty() {
        return rendered.to_string();
    }
    if rendered.starts_with(buffered) || rendered.ends_with(buffered) {
        return rendered.to_string();
    }
    if buffered.starts_with(rendered) || buffered.ends_with(rendered) {
        return buffered.to_string();
    }

    let overlap = buffered
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(buffered.len()))
        .filter(|index| *index > 0 && rendered.ends_with(&buffered[..*index]))
        .max()
        .unwrap_or(0);
    if overlap > 0 {
        format!("{rendered}{}", &buffered[overlap..])
    } else {
        rendered.to_string()
    }
}

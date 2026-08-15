pub mod custom_blocks;
pub mod element;
pub mod highlight;
pub mod input;

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
        mpsc::{SendError, Sender},
    },
};

use alacritty_terminal::{
    event::{Event, EventListener},
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point, Side},
    selection::{Selection, SelectionRange, SelectionType},
    term::{Config, Term, TermMode, cell::Cell, point_to_viewport, viewport_to_point},
    vte::ansi::{CursorShape, Processor},
};
use gpui::Keystroke;

use crate::session::config::Session;
use crate::sftp::{PreviewData, RemoteEntry};
use crate::system::{RemotePort, RemoteProcess, SystemSnapshot};
use crate::text_encoding::{StreamingDecoder, TextEncoding};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabKind {
    Local,
    Ssh,
    Serial,
}

#[derive(Debug)]
pub enum BackendCommand {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    SampleMetrics,
    SampleProcesses,
    SamplePorts,
    TerminateProcess { pid: u32 },
    Close,
}

#[derive(Debug, Clone)]
pub enum BackendEvent {
    Guarded {
        current_generation: Arc<AtomicU32>,
        generation: u32,
        event: Box<BackendEvent>,
    },
    Output {
        tab_id: String,
        bytes: Vec<u8>,
    },
    Status {
        tab_id: String,
        text: String,
    },
    Connected {
        tab_id: String,
    },
    SftpEntries {
        tab_id: String,
        path: String,
        entries: Vec<RemoteEntry>,
    },
    SftpDirectoryFailed {
        tab_id: String,
        path: String,
        reason: String,
    },
    SftpPreview {
        tab_id: String,
        preview: PreviewData,
    },
    SftpStatus {
        tab_id: String,
        text: String,
    },
    RemoteSystem {
        tab_id: String,
        snapshot: SystemSnapshot,
    },
    RemoteSystemUnavailable {
        tab_id: String,
        reason: String,
    },
    RemoteProcesses {
        tab_id: String,
        processes: Vec<RemoteProcess>,
    },
    RemoteProcessesUnavailable {
        tab_id: String,
        reason: String,
    },
    RemotePorts {
        tab_id: String,
        ports: Vec<RemotePort>,
    },
    RemotePortsUnavailable {
        tab_id: String,
        reason: String,
    },
    RemoteProcessTerminated {
        tab_id: String,
        pid: u32,
    },
    RemoteProcessTerminateFailed {
        tab_id: String,
        pid: u32,
        reason: String,
    },
    SftpHome {
        tab_id: String,
        home: String,
    },
    TransferProgress {
        #[allow(dead_code)]
        tab_id: String,
        id: String,
        transferred: u64,
        total: Option<u64>,
        state: TransferState,
    },
    TransferStarted {
        tab_id: String,
        info: TransferInfo,
    },
    Closed {
        tab_id: String,
        reason: String,
    },
    TerminalTitleChanged {
        tab_id: String,
        title: String,
    },
    LocalDirectoryChanged {
        tab_id: String,
        path: std::path::PathBuf,
    },
    SyncFinished(crate::sync::SyncResult),
}

impl BackendEvent {
    pub(crate) fn into_current(self) -> Option<Self> {
        match self {
            Self::Guarded {
                current_generation,
                generation,
                event,
            } if current_generation.load(Ordering::Acquire) == generation => Some(*event),
            Self::Guarded { .. } => None,
            event => Some(event),
        }
    }
}

/// Filters events emitted by superseded terminal backends.
///
/// Every backend instance captures the tab generation that was current when it
/// was spawned. Reconnecting advances the shared generation before the old
/// backend is closed, so late events from that backend never reach the UI.
#[derive(Clone)]
pub struct GuardedBackendEventSender {
    events: Sender<BackendEvent>,
    current_generation: Arc<AtomicU32>,
    generation: u32,
}

impl GuardedBackendEventSender {
    pub fn new(events: Sender<BackendEvent>) -> Self {
        Self {
            events,
            current_generation: Arc::new(AtomicU32::new(0)),
            generation: 0,
        }
    }

    pub fn send(&self, event: BackendEvent) -> Result<(), Box<SendError<BackendEvent>>> {
        if self.current_generation.load(Ordering::Acquire) != self.generation {
            return Ok(());
        }
        self.events
            .send(BackendEvent::Guarded {
                current_generation: self.current_generation.clone(),
                generation: self.generation,
                event: Box::new(event),
            })
            .map_err(Box::new)
    }

    fn next_generation(&self) -> Self {
        let generation = self
            .current_generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        Self {
            events: self.events.clone(),
            current_generation: self.current_generation.clone(),
            generation,
        }
    }

    fn unguarded_sender(&self) -> Sender<BackendEvent> {
        self.events.clone()
    }
}

#[derive(Clone)]
pub enum BackendTx {
    Local(Sender<BackendCommand>),
    Ssh(tokio::sync::mpsc::UnboundedSender<BackendCommand>),
    Serial(tokio::sync::mpsc::UnboundedSender<BackendCommand>),
    /// A restored session that is waiting for the user to confirm reconnecting.
    Pending,
}

impl BackendTx {
    pub fn send(&self, command: BackendCommand) {
        match self {
            Self::Local(tx) => {
                let _ = tx.send(command);
            }
            Self::Ssh(tx) => {
                let _ = tx.send(command);
            }
            Self::Serial(tx) => {
                let _ = tx.send(command);
            }
            Self::Pending => {}
        }
    }
}

pub struct TerminalTab {
    pub id: String,
    pub title: String,
    pub dynamic_title: String,
    pub terminal_title_received: bool,
    pub local_cwd: Option<PathBuf>,
    pub kind: TabKind,
    pub status: String,
    pub connected: bool,
    pub disconnected_reason: Option<String>,
    pub session: Option<Session>,
    text_encoding: TextEncoding,
    output_decoder: StreamingDecoder,
    processor: Processor,
    term: Term<TerminalListener>,
    pub cols: u16,
    pub rows: u16,
    pub backend: std::sync::Arc<std::sync::Mutex<BackendTx>>,
    backend_events: GuardedBackendEventSender,
    pub scroll_pixel_y: f32,
    pub(crate) highlight_cache: HighlightCache,
}

type HighlightCache = std::cell::RefCell<
    Option<(
        Vec<RenderCell>,
        std::collections::HashMap<(i32, i32), gpui::Hsla>,
    )>,
>;

#[derive(Clone, Copy)]
pub struct CursorState {
    pub row: usize,
    pub col: usize,
    pub shape: CursorShape,
}

#[derive(Clone, PartialEq)]
pub struct RenderCell {
    pub row: i32,
    pub col: i32,
    pub cell: Cell,
}

#[derive(Clone)]
pub struct RenderSnapshot {
    pub cells: Vec<RenderCell>,
    pub cursor: Option<CursorState>,
    pub selection: Option<ViewportSelection>,
    pub display_offset: usize,
    pub history_size: usize,
    pub rows: usize,
    pub cols: usize,
    pub highlights: std::collections::HashMap<(i32, i32), gpui::Hsla>,
}

#[derive(Clone, Copy)]
pub struct ViewportSelection {
    pub start_row: usize,
    pub start_col: usize,
    pub end_row: usize,
    pub end_col: usize,
    pub is_block: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct SftpTreeRow {
    pub(crate) path: String,
    pub(crate) label: String,
    pub(crate) depth: usize,
    pub(crate) expanded: bool,
    pub(crate) loading: bool,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Default)]
pub struct SftpUiState {
    pub current_path: String,
    pub status: String,
    pub directory_cache: HashMap<String, Vec<RemoteEntry>>,
    pub expanded_directories: HashSet<String>,
    pub loading_directories: HashSet<String>,
    pub directory_errors: HashMap<String, String>,
    pub selected_path: Option<String>,
    pub preview: Option<PreviewData>,
    pub selected_entries: HashSet<String>,
    pub home_dir: String,
    pub home_dir_resolved: bool,
}

impl SftpUiState {
    pub(crate) fn current_entries(&self) -> &[RemoteEntry] {
        self.directory_cache
            .get(&self.current_path)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn begin_directory_load(&mut self, path: &str) {
        self.loading_directories.insert(path.to_string());
        self.directory_errors.remove(path);
    }

    pub(crate) fn apply_directory_entries(&mut self, path: String, entries: Vec<RemoteEntry>) {
        self.loading_directories.remove(&path);
        self.directory_errors.remove(&path);
        self.directory_cache.insert(path, entries);
    }

    pub(crate) fn apply_directory_error(&mut self, path: String, reason: String) {
        self.loading_directories.remove(&path);
        self.directory_errors.insert(path, reason);
    }

    pub(crate) fn expand_to(&mut self, path: &str) {
        self.expanded_directories
            .extend(crate::sftp::remote_path_ancestors(path));
    }

    pub(crate) fn collapse_all(&mut self) {
        self.expanded_directories.clear();
        self.expanded_directories.insert("/".to_string());
    }

    pub(crate) fn tree_rows(&self, show_hidden: bool) -> Vec<SftpTreeRow> {
        fn append_rows(
            rows: &mut Vec<SftpTreeRow>,
            state: &SftpUiState,
            path: String,
            label: String,
            depth: usize,
            show_hidden: bool,
        ) {
            let visible_directories = state.directory_cache.get(&path).map(|entries| {
                entries
                    .iter()
                    .filter(|entry| entry.is_dir && (show_hidden || !entry.name.starts_with('.')))
                    .cloned()
                    .collect::<Vec<_>>()
            });
            let expanded = state.expanded_directories.contains(&path);

            rows.push(SftpTreeRow {
                path: path.clone(),
                label,
                depth,
                expanded,
                loading: state.loading_directories.contains(&path),
                error: state.directory_errors.get(&path).cloned(),
            });

            if !expanded {
                return;
            }
            if let Some(directories) = visible_directories {
                for directory in directories {
                    append_rows(
                        rows,
                        state,
                        directory.full_path,
                        directory.name,
                        depth + 1,
                        show_hidden,
                    );
                }
            }
        }

        let mut rows = Vec::new();
        append_rows(
            &mut rows,
            self,
            "/".to_string(),
            "/".to_string(),
            0,
            show_hidden,
        );
        rows
    }
}

#[cfg(test)]
mod sftp_ui_tests {
    use super::SftpUiState;
    use crate::sftp::RemoteEntry;

    fn directory(name: &str, path: &str) -> RemoteEntry {
        RemoteEntry {
            name: name.to_string(),
            full_path: path.to_string(),
            is_dir: true,
            size: 0,
            modified: 0,
        }
    }

    #[test]
    fn directory_responses_do_not_replace_the_current_path() {
        let mut state = SftpUiState {
            current_path: "/home/demo".to_string(),
            ..SftpUiState::default()
        };

        state.apply_directory_entries("/var".to_string(), vec![directory("log", "/var/log")]);

        assert_eq!(state.current_path, "/home/demo");
        assert_eq!(state.directory_cache["/var"][0].full_path, "/var/log");
    }

    #[test]
    fn tree_rows_follow_expansion_and_hidden_directory_preferences() {
        let mut state = SftpUiState::default();
        state.expanded_directories.insert("/".to_string());
        state.expanded_directories.insert("/home".to_string());
        state.apply_directory_entries(
            "/".to_string(),
            vec![
                directory(".internal", "/.internal"),
                directory("home", "/home"),
            ],
        );
        state.apply_directory_entries("/home".to_string(), vec![directory("demo", "/home/demo")]);

        let visible_paths = state
            .tree_rows(false)
            .into_iter()
            .map(|row| row.path)
            .collect::<Vec<_>>();
        assert_eq!(visible_paths, vec!["/", "/home", "/home/demo"]);

        let visible_with_hidden = state
            .tree_rows(true)
            .into_iter()
            .map(|row| row.path)
            .collect::<Vec<_>>();
        assert_eq!(
            visible_with_hidden,
            vec!["/", "/.internal", "/home", "/home/demo"]
        );
    }
}

#[cfg(test)]
mod backend_event_tests {
    use super::{BackendEvent, GuardedBackendEventSender};

    #[test]
    fn superseded_backend_events_are_discarded() {
        let (events, received) = std::sync::mpsc::channel();
        let first = GuardedBackendEventSender::new(events);
        first
            .send(BackendEvent::Closed {
                tab_id: "tab-1".to_string(),
                reason: "queued stale close".to_string(),
            })
            .unwrap();

        let second = first.next_generation();
        first
            .send(BackendEvent::Output {
                tab_id: "tab-1".to_string(),
                bytes: b"late stale output".to_vec(),
            })
            .unwrap();
        second
            .send(BackendEvent::Connected {
                tab_id: "tab-1".to_string(),
            })
            .unwrap();

        assert!(received.recv().unwrap().into_current().is_none());
        assert!(matches!(
            received.recv().unwrap().into_current(),
            Some(BackendEvent::Connected { .. })
        ));
        assert!(received.try_recv().is_err());
    }
}

impl TerminalTab {
    pub fn new_local(
        id: String,
        title: String,
        backend: BackendTx,
        backend_events: GuardedBackendEventSender,
    ) -> Self {
        Self::new(
            id,
            title,
            TabKind::Local,
            "local shell".into(),
            backend,
            backend_events,
        )
    }

    pub fn new_ssh(
        id: String,
        session: &Session,
        backend: BackendTx,
        backend_events: GuardedBackendEventSender,
    ) -> Self {
        let mut tab = Self::new(
            id,
            session.name.clone(),
            TabKind::Ssh,
            format!(
                "connecting {}@{}:{}",
                session.user, session.host, session.port
            ),
            backend,
            backend_events,
        );
        tab.session = Some(session.clone());
        tab.set_text_encoding(session.terminal_encoding);
        tab.connected = false;
        tab
    }

    pub fn new_serial(
        id: String,
        session: &Session,
        backend: BackendTx,
        backend_events: GuardedBackendEventSender,
    ) -> Self {
        let mut tab = Self::new(
            id,
            session.name.clone(),
            TabKind::Serial,
            format!("connecting serial://{}@{}", session.host, session.baud_rate),
            backend,
            backend_events,
        );
        tab.session = Some(session.clone());
        tab.connected = false;
        tab
    }

    fn new(
        id: String,
        title: String,
        kind: TabKind,
        status: String,
        backend: BackendTx,
        backend_events: GuardedBackendEventSender,
    ) -> Self {
        let shared_backend = std::sync::Arc::new(std::sync::Mutex::new(backend));
        let events = backend_events.unguarded_sender();
        Self {
            id: id.clone(),
            title: title.clone(),
            dynamic_title: title,
            terminal_title_received: false,
            local_cwd: None,
            kind,
            status,
            connected: matches!(kind, TabKind::Local),
            disconnected_reason: None,
            session: None,
            text_encoding: TextEncoding::Utf8,
            output_decoder: StreamingDecoder::new(TextEncoding::Utf8),
            processor: Processor::new(),
            term: new_term(100, 30, shared_backend.clone(), id, events.clone()),
            cols: 100,
            rows: 30,
            backend: shared_backend,
            backend_events,
            scroll_pixel_y: 0.0,
            highlight_cache: std::cell::RefCell::new(None),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        let decoded = self.output_decoder.decode(bytes);
        self.processor.advance(&mut self.term, &decoded);
    }

    pub(crate) fn text_encoding(&self) -> TextEncoding {
        self.text_encoding
    }

    pub(crate) fn set_text_encoding(&mut self, encoding: TextEncoding) {
        if self.text_encoding == encoding {
            return;
        }
        self.text_encoding = encoding;
        self.output_decoder = StreamingDecoder::new(encoding);
        if let Some(session) = self.session.as_mut() {
            session.terminal_encoding = encoding;
        }
    }

    pub(crate) fn encode_input(&self, bytes: &[u8]) -> Vec<u8> {
        self.text_encoding.encode_terminal_input(bytes).into_owned()
    }

    /// Send a command to the backend. Thread-safe via the shared Arc<Mutex>.
    pub fn send_backend(&self, command: BackendCommand) {
        if let Ok(backend) = self.backend.lock() {
            backend.send(command);
        }
    }

    /// Replace the backend with a new one. The `Term`'s internal listener
    /// shares the same `Arc`, so user input is automatically routed to the
    /// new backend. The old backend must be closed by the caller.
    pub fn set_backend(&self, new_backend: BackendTx) {
        if let Ok(mut backend) = self.backend.lock() {
            *backend = new_backend;
        }
    }

    /// Advances this tab to a new backend generation and returns its sender.
    /// Call this before closing the old backend so all of its remaining events
    /// are discarded immediately.
    pub fn advance_backend_events(&mut self) -> GuardedBackendEventSender {
        let next = self.backend_events.next_generation();
        self.backend_events = next.clone();
        next
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let new_cols = cols.max(1);
        let new_rows = rows.max(1);
        if self.cols != new_cols || self.rows != new_rows {
            self.cols = new_cols;
            self.rows = new_rows;
            tracing::info!(
                "[ui] terminal resized to {}x{} (cols x rows)",
                self.cols,
                self.rows
            );
            self.term.resize(TerminalSize::new(self.cols, self.rows));
            self.send_backend(BackendCommand::Resize { cols, rows });
        }
    }

    pub fn cursor_state(&self) -> Option<CursorState> {
        let content = self.term.renderable_content();
        if matches!(content.cursor.shape, CursorShape::Hidden) || content.display_offset > 0 {
            return None;
        }
        let row = content.cursor.point.line.0;
        if row < 0 {
            return None;
        }
        let row = row as usize;
        if row >= self.rows as usize {
            return None;
        }

        Some(CursorState {
            row,
            col: content.cursor.point.column.0,
            shape: content.cursor.shape,
        })
    }

    pub fn app_cursor_mode(&self) -> bool {
        self.term.mode().contains(TermMode::APP_CURSOR)
    }

    pub fn is_alternate_screen_active(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    pub fn render_snapshot(&self, keyword_highlight: bool) -> RenderSnapshot {
        let rows = self.rows;
        let cols = self.cols;
        let content = self.term.renderable_content();
        let display_offset = content.display_offset as i32;
        let mut cells = Vec::with_capacity((rows as usize) * (cols as usize));

        for indexed in content.display_iter {
            let line = indexed.point.line.0;
            let row = line + display_offset;
            if row < 0 {
                continue;
            }
            if row >= rows as i32 {
                continue;
            }

            let col = indexed.point.column.0 as i32;
            if col >= cols as i32 {
                continue;
            }

            cells.push(RenderCell {
                row,
                col,
                cell: indexed.cell.clone(),
            });
        }

        // Get highlights from cache or recompute, only if keyword_highlight is enabled.
        let is_enabled = keyword_highlight;

        let highlights = if is_enabled {
            let mut cache = self.highlight_cache.borrow_mut();
            let cache_valid = cache
                .as_ref()
                .is_some_and(|(cached_cells, _)| cached_cells == &cells);
            if cache_valid {
                cache.as_ref().unwrap().1.clone()
            } else {
                let computed = self::highlight::highlight_cells(&cells, rows as usize);
                *cache = Some((cells.clone(), computed.clone()));
                computed
            }
        } else {
            std::collections::HashMap::new()
        };

        RenderSnapshot {
            cells,
            cursor: self.cursor_state(),
            selection: viewport_selection_from_range(
                content.display_offset,
                self.rows as usize,
                self.cols as usize,
                &content.selection,
            ),
            display_offset: content.display_offset,
            history_size: self.term.grid().history_size(),
            rows: self.rows as usize,
            cols: self.cols as usize,
            highlights,
        }
    }

    /// Return `(grid_line_base, rows_data)` for the **entire** terminal buffer
    /// including scrollback history. `grid_line_base` is the grid line index of
    /// the first row (typically `-history_size`). Each entry in `rows_data` is
    /// a sorted `Vec<(col, char)>` for that row.
    pub fn full_grid_rows(&self) -> (i32, Vec<Vec<(i32, char)>>) {
        let grid = self.term.grid();
        let history = grid.history_size() as i32;
        let screen = grid.screen_lines() as i32;
        let total = history + screen;
        let cols = self.cols as i32;
        let start_line = -history;

        let mut rows_data: Vec<Vec<(i32, char)>> = Vec::with_capacity(total as usize);
        for line_idx in start_line..(start_line + total) {
            let line = Line(line_idx);
            let mut cells: Vec<(i32, char)> = Vec::new();
            for col_idx in 0..cols {
                let point = Point::new(line, Column(col_idx as usize));
                let c = grid[point].c;
                if c != ' ' && c != '\0' {
                    cells.push((col_idx, c));
                }
            }
            rows_data.push(cells);
        }
        (start_line, rows_data)
    }

    pub fn scroll_history(&mut self, delta: i32) {
        if delta != 0 {
            self.term.scroll_display(Scroll::Delta(delta));
        }
    }

    pub fn scroll_up_by(&mut self, lines: usize) {
        if lines != 0 {
            self.term.scroll_display(Scroll::Delta(lines as i32));
        }
    }

    pub fn scroll_down_by(&mut self, lines: usize) {
        if lines != 0 {
            self.term.scroll_display(Scroll::Delta(-(lines as i32)));
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    #[allow(dead_code)]
    pub fn has_selection(&self) -> bool {
        self.term
            .selection_to_string()
            .is_some_and(|text| !text.is_empty())
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    pub fn selection_text(&self) -> Option<String> {
        self.term
            .selection_to_string()
            .filter(|text| !text.is_empty())
    }

    pub fn begin_selection(
        &mut self,
        row: usize,
        col: usize,
        side: Side,
        selection_type: SelectionType,
    ) {
        let point = viewport_to_point(
            self.term.grid().display_offset(),
            Point::new(row, Column(col)),
        );
        self.term.selection = Some(Selection::new(selection_type, point, side));
    }

    pub fn update_selection(&mut self, row: usize, col: usize, side: Side) {
        let point = viewport_to_point(
            self.term.grid().display_offset(),
            Point::new(row, Column(col)),
        );
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, side);
        }
    }

    pub fn paste_text(&mut self, text: &str) {
        let bracketed = self.term.mode().contains(TermMode::BRACKETED_PASTE);
        let paste_text = text
            .replace('\x1b', "")
            .replace("\r\n", "\r")
            .replace('\n', "\r");

        let mut bytes = Vec::new();
        if bracketed {
            bytes.extend_from_slice(b"\x1b[200~");
        }
        bytes.extend_from_slice(&self.encode_input(paste_text.as_bytes()));
        if bracketed {
            bytes.extend_from_slice(b"\x1b[201~");
        }

        self.send_backend(BackendCommand::Input(bytes));
    }
}

fn viewport_selection_from_range(
    display_offset: usize,
    rows: usize,
    cols: usize,
    selection: &Option<SelectionRange>,
) -> Option<ViewportSelection> {
    let SelectionRange {
        start,
        end,
        is_block,
    } = selection.as_ref().copied()?;

    let top_point = viewport_to_point(display_offset, Point::new(0, Column(0)));
    let bottom_point = viewport_to_point(
        display_offset,
        Point::new(rows.saturating_sub(1), Column(0)),
    );

    let top_line = top_point.line;
    let bottom_line = bottom_point.line;

    let start_vp = if start.line < top_line {
        Point::new(0, Column(0))
    } else if start.line > bottom_line {
        Point::new(rows.saturating_sub(1), Column(cols.saturating_sub(1)))
    } else {
        point_to_viewport(display_offset, start).unwrap_or(Point::new(0, Column(0)))
    };

    let end_vp = if end.line < top_line {
        Point::new(0, Column(0))
    } else if end.line > bottom_line {
        Point::new(rows.saturating_sub(1), Column(cols.saturating_sub(1)))
    } else {
        point_to_viewport(display_offset, end).unwrap_or(Point::new(
            rows.saturating_sub(1),
            Column(cols.saturating_sub(1)),
        ))
    };

    Some(ViewportSelection {
        start_row: start_vp.line,
        start_col: start_vp.column.0,
        end_row: end_vp.line,
        end_col: end_vp.column.0,
        is_block,
    })
}

#[derive(Clone)]
struct TerminalListener {
    tab_id: String,
    backend: std::sync::Arc<std::sync::Mutex<BackendTx>>,
    events: std::sync::mpsc::Sender<BackendEvent>,
}

impl EventListener for TerminalListener {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(output) => {
                if let Ok(backend) = self.backend.lock() {
                    backend.send(BackendCommand::Input(output.into_bytes()));
                }
            }
            Event::TextAreaSizeRequest(format) => {
                let size = alacritty_terminal::event::WindowSize {
                    num_lines: 30,
                    num_cols: 100,
                    cell_width: 8,
                    cell_height: 16,
                };
                if let Ok(backend) = self.backend.lock() {
                    backend.send(BackendCommand::Input(format(size).into_bytes()));
                }
            }
            Event::Title(title) => {
                let _ = self.events.send(BackendEvent::TerminalTitleChanged {
                    tab_id: self.tab_id.clone(),
                    title,
                });
            }
            _ => {}
        }
    }
}

fn new_term(
    cols: u16,
    rows: u16,
    backend: std::sync::Arc<std::sync::Mutex<BackendTx>>,
    tab_id: String,
    events: std::sync::mpsc::Sender<BackendEvent>,
) -> Term<TerminalListener> {
    Term::new(
        Config {
            scrolling_history: 2000,
            ..Config::default()
        },
        &TerminalSize::new(cols, rows),
        TerminalListener {
            tab_id,
            backend,
            events,
        },
    )
}

struct TerminalSize {
    cols: usize,
    rows: usize,
}

impl TerminalSize {
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols: cols.max(1) as usize,
            rows: rows.max(1) as usize,
        }
    }
}

impl Dimensions for TerminalSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

pub fn encode_key(
    keystroke: &Keystroke,
    app_cursor_mode: bool,
    option_as_meta: bool,
) -> Option<Vec<u8>> {
    zed_like_to_esc_str(keystroke, app_cursor_mode, option_as_meta)
        .map(|text| text.into_owned().into_bytes())
}

#[derive(Debug, PartialEq, Eq)]
enum TerminalModifiers {
    None,
    Alt,
    Ctrl,
    Shift,
    CtrlShift,
    Other,
}

impl TerminalModifiers {
    fn new(ks: &Keystroke) -> Self {
        match (
            ks.modifiers.alt,
            ks.modifiers.control,
            ks.modifiers.shift,
            ks.modifiers.platform,
        ) {
            (false, false, false, false) => Self::None,
            (true, false, false, false) => Self::Alt,
            (false, true, false, false) => Self::Ctrl,
            (false, false, true, false) => Self::Shift,
            (false, true, true, false) => Self::CtrlShift,
            _ => Self::Other,
        }
    }

    fn any(&self) -> bool {
        !matches!(self, Self::None)
    }
}

fn zed_like_to_esc_str(
    keystroke: &Keystroke,
    app_cursor_mode: bool,
    option_as_meta: bool,
) -> Option<std::borrow::Cow<'static, str>> {
    let modifiers = TerminalModifiers::new(keystroke);
    let key = keystroke.key.to_ascii_lowercase();

    let manual_esc_str = match (key.as_str(), &modifiers) {
        ("tab", TerminalModifiers::None) => Some("\x09"),
        ("tab", TerminalModifiers::Shift) => Some("\x1b[Z"),
        ("escape", TerminalModifiers::None) => Some("\x1b"),
        ("enter", TerminalModifiers::None) => Some("\x0d"),
        ("enter", TerminalModifiers::Shift) => Some("\x0a"),
        ("enter", TerminalModifiers::Alt) => Some("\x1b\x0d"),
        ("backspace", TerminalModifiers::None) => Some("\x7f"),
        ("backspace", TerminalModifiers::Ctrl) => Some("\x08"),
        ("backspace", TerminalModifiers::Alt) => Some("\x1b\x7f"),
        ("backspace", TerminalModifiers::Shift) => Some("\x7f"),
        ("space", TerminalModifiers::Ctrl) => Some("\x00"),
        ("home", TerminalModifiers::None) if app_cursor_mode => Some("\x1bOH"),
        ("home", TerminalModifiers::None) if !app_cursor_mode => Some("\x1b[H"),
        ("end", TerminalModifiers::None) if app_cursor_mode => Some("\x1bOF"),
        ("end", TerminalModifiers::None) if !app_cursor_mode => Some("\x1b[F"),
        ("up", TerminalModifiers::None) if app_cursor_mode => Some("\x1bOA"),
        ("up", TerminalModifiers::None) if !app_cursor_mode => Some("\x1b[A"),
        ("down", TerminalModifiers::None) if app_cursor_mode => Some("\x1bOB"),
        ("down", TerminalModifiers::None) if !app_cursor_mode => Some("\x1b[B"),
        ("right", TerminalModifiers::None) if app_cursor_mode => Some("\x1bOC"),
        ("right", TerminalModifiers::None) if !app_cursor_mode => Some("\x1b[C"),
        ("left", TerminalModifiers::None) if app_cursor_mode => Some("\x1bOD"),
        ("left", TerminalModifiers::None) if !app_cursor_mode => Some("\x1b[D"),
        ("insert", TerminalModifiers::None) => Some("\x1b[2~"),
        ("delete", TerminalModifiers::None) => Some("\x1b[3~"),
        ("pageup", TerminalModifiers::None) => Some("\x1b[5~"),
        ("pagedown", TerminalModifiers::None) => Some("\x1b[6~"),
        ("a", TerminalModifiers::Ctrl) | ("A", TerminalModifiers::CtrlShift) => Some("\x01"),
        ("b", TerminalModifiers::Ctrl) | ("B", TerminalModifiers::CtrlShift) => Some("\x02"),
        ("c", TerminalModifiers::Ctrl) | ("C", TerminalModifiers::CtrlShift) => Some("\x03"),
        ("d", TerminalModifiers::Ctrl) | ("D", TerminalModifiers::CtrlShift) => Some("\x04"),
        ("e", TerminalModifiers::Ctrl) | ("E", TerminalModifiers::CtrlShift) => Some("\x05"),
        ("f", TerminalModifiers::Ctrl) | ("F", TerminalModifiers::CtrlShift) => Some("\x06"),
        ("g", TerminalModifiers::Ctrl) | ("G", TerminalModifiers::CtrlShift) => Some("\x07"),
        ("h", TerminalModifiers::Ctrl) | ("H", TerminalModifiers::CtrlShift) => Some("\x08"),
        ("i", TerminalModifiers::Ctrl) | ("I", TerminalModifiers::CtrlShift) => Some("\x09"),
        ("j", TerminalModifiers::Ctrl) | ("J", TerminalModifiers::CtrlShift) => Some("\x0a"),
        ("k", TerminalModifiers::Ctrl) | ("K", TerminalModifiers::CtrlShift) => Some("\x0b"),
        ("l", TerminalModifiers::Ctrl) | ("L", TerminalModifiers::CtrlShift) => Some("\x0c"),
        ("m", TerminalModifiers::Ctrl) | ("M", TerminalModifiers::CtrlShift) => Some("\x0d"),
        ("n", TerminalModifiers::Ctrl) | ("N", TerminalModifiers::CtrlShift) => Some("\x0e"),
        ("o", TerminalModifiers::Ctrl) | ("O", TerminalModifiers::CtrlShift) => Some("\x0f"),
        ("p", TerminalModifiers::Ctrl) | ("P", TerminalModifiers::CtrlShift) => Some("\x10"),
        ("q", TerminalModifiers::Ctrl) | ("Q", TerminalModifiers::CtrlShift) => Some("\x11"),
        ("r", TerminalModifiers::Ctrl) | ("R", TerminalModifiers::CtrlShift) => Some("\x12"),
        ("s", TerminalModifiers::Ctrl) | ("S", TerminalModifiers::CtrlShift) => Some("\x13"),
        ("t", TerminalModifiers::Ctrl) | ("T", TerminalModifiers::CtrlShift) => Some("\x14"),
        ("u", TerminalModifiers::Ctrl) | ("U", TerminalModifiers::CtrlShift) => Some("\x15"),
        ("v", TerminalModifiers::Ctrl) | ("V", TerminalModifiers::CtrlShift) => Some("\x16"),
        ("w", TerminalModifiers::Ctrl) | ("W", TerminalModifiers::CtrlShift) => Some("\x17"),
        ("x", TerminalModifiers::Ctrl) | ("X", TerminalModifiers::CtrlShift) => Some("\x18"),
        ("y", TerminalModifiers::Ctrl) | ("Y", TerminalModifiers::CtrlShift) => Some("\x19"),
        ("z", TerminalModifiers::Ctrl) | ("Z", TerminalModifiers::CtrlShift) => Some("\x1a"),
        ("@", TerminalModifiers::Ctrl) => Some("\x00"),
        ("[", TerminalModifiers::Ctrl) => Some("\x1b"),
        ("\\", TerminalModifiers::Ctrl) => Some("\x1c"),
        ("]", TerminalModifiers::Ctrl) => Some("\x1d"),
        ("^", TerminalModifiers::Ctrl) => Some("\x1e"),
        ("_", TerminalModifiers::Ctrl) => Some("\x1f"),
        ("?", TerminalModifiers::Ctrl) => Some("\x7f"),
        ("f1", TerminalModifiers::None) => Some("\x1bOP"),
        ("f2", TerminalModifiers::None) => Some("\x1bOQ"),
        ("f3", TerminalModifiers::None) => Some("\x1bOR"),
        ("f4", TerminalModifiers::None) => Some("\x1bOS"),
        ("f5", TerminalModifiers::None) => Some("\x1b[15~"),
        ("f6", TerminalModifiers::None) => Some("\x1b[17~"),
        ("f7", TerminalModifiers::None) => Some("\x1b[18~"),
        ("f8", TerminalModifiers::None) => Some("\x1b[19~"),
        ("f9", TerminalModifiers::None) => Some("\x1b[20~"),
        ("f10", TerminalModifiers::None) => Some("\x1b[21~"),
        ("f11", TerminalModifiers::None) => Some("\x1b[23~"),
        ("f12", TerminalModifiers::None) => Some("\x1b[24~"),
        _ => None,
    };
    if let Some(esc) = manual_esc_str {
        return Some(esc.into());
    }

    if modifiers.any() {
        let modifier_code = modifier_code(keystroke);
        let modified = match key.as_str() {
            "up" => Some(format!("\x1b[1;{}A", modifier_code)),
            "down" => Some(format!("\x1b[1;{}B", modifier_code)),
            "right" => Some(format!("\x1b[1;{}C", modifier_code)),
            "left" => Some(format!("\x1b[1;{}D", modifier_code)),
            "insert" => Some(format!("\x1b[2;{}~", modifier_code)),
            "pageup" => Some(format!("\x1b[5;{}~", modifier_code)),
            "pagedown" => Some(format!("\x1b[6;{}~", modifier_code)),
            "end" => Some(format!("\x1b[1;{}F", modifier_code)),
            "home" => Some(format!("\x1b[1;{}H", modifier_code)),
            "f1" => Some(format!("\x1b[1;{}P", modifier_code)),
            "f2" => Some(format!("\x1b[1;{}Q", modifier_code)),
            "f3" => Some(format!("\x1b[1;{}R", modifier_code)),
            "f4" => Some(format!("\x1b[1;{}S", modifier_code)),
            "f5" => Some(format!("\x1b[15;{}~", modifier_code)),
            "f6" => Some(format!("\x1b[17;{}~", modifier_code)),
            "f7" => Some(format!("\x1b[18;{}~", modifier_code)),
            "f8" => Some(format!("\x1b[19;{}~", modifier_code)),
            "f9" => Some(format!("\x1b[20;{}~", modifier_code)),
            "f10" => Some(format!("\x1b[21;{}~", modifier_code)),
            "f11" => Some(format!("\x1b[23;{}~", modifier_code)),
            "f12" => Some(format!("\x1b[24;{}~", modifier_code)),
            _ => None,
        };
        if let Some(esc) = modified {
            return Some(esc.into());
        }
    }

    if !cfg!(target_os = "macos") || option_as_meta {
        let is_alt_lowercase_ascii =
            modifiers == TerminalModifiers::Alt && keystroke.key.is_ascii();
        let is_alt_uppercase_ascii =
            keystroke.modifiers.alt && keystroke.modifiers.shift && keystroke.key.is_ascii();
        if is_alt_lowercase_ascii || is_alt_uppercase_ascii {
            let key = if is_alt_uppercase_ascii {
                keystroke.key.to_ascii_uppercase()
            } else {
                keystroke.key.clone()
            };
            return Some(format!("\x1b{}", key).into());
        }
    }

    if let Some(text) = &keystroke.key_char {
        return Some(text.clone().into());
    }

    if keystroke.key.len() == 1 {
        return Some(keystroke.key.clone().into());
    }

    None
}

fn modifier_code(keystroke: &Keystroke) -> u32 {
    let mut modifier_code = 0;
    if keystroke.modifiers.shift {
        modifier_code |= 1;
    }
    if keystroke.modifiers.alt {
        modifier_code |= 1 << 1;
    }
    if keystroke.modifiers.control {
        modifier_code |= 1 << 2;
    }
    modifier_code + 1
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TransferType {
    Upload,
    Download,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum TransferState {
    Running,
    Paused,
    Completed,
    Failed(String),
    Interrupted(String), // 中断传输：包含原因（例如 "User cancelled", "Network timeout"）
    Zombie(String),      // 程序重启后残留的 Running/Paused 任务
                         // 兼容 v0.3.11 -> v0.4.x：旧配置里曾保存过 `Cancelled`，
                         // 新版本改成了带原因的状态，因此要手动接住旧枚举值。
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
enum TransferStateCompat {
    Running,
    Paused,
    Completed,
    Failed(String),
    Interrupted(String),
    Zombie(String),
    Cancelled,
}

impl From<TransferStateCompat> for TransferState {
    fn from(value: TransferStateCompat) -> Self {
        match value {
            TransferStateCompat::Running => Self::Running,
            TransferStateCompat::Paused => Self::Paused,
            TransferStateCompat::Completed => Self::Completed,
            TransferStateCompat::Failed(reason) => Self::Failed(reason),
            TransferStateCompat::Interrupted(reason) => Self::Interrupted(reason),
            TransferStateCompat::Zombie(reason) => Self::Zombie(reason),
            TransferStateCompat::Cancelled => Self::Interrupted("Cancelled".to_string()),
        }
    }
}

impl<'de> serde::Deserialize<'de> for TransferState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        TransferStateCompat::deserialize(deserializer).map(Into::into)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TransferInfo {
    pub id: String,
    pub name: String,
    pub source: String,
    pub target: String,
    pub kind: TransferType,
    pub total_bytes: Option<u64>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Transfer {
    pub tab_id: String,
    pub tab_title: String,
    pub info: TransferInfo,
    pub transferred: u64,
    pub total: Option<u64>,
    pub state: TransferState,
}

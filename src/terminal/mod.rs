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
    time::{Duration, Instant},
};

use alacritty_terminal::{
    event::{Event, EventListener},
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point, Side},
    selection::{Selection, SelectionRange, SelectionType},
    term::{Config, Term, TermMode, cell::Cell, point_to_viewport, viewport_to_point},
    vte::ansi::{CursorShape, Processor},
};
use base64::{Engine as _, engine::general_purpose};
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

const TERMINAL_ACTIVITY_GRACE: Duration = Duration::from_millis(750);
const MAX_OSC_PAYLOAD_BYTES: usize = 4096;
const MAX_NOTIFICATION_TEXT_BYTES: usize = 8192;
const MAX_OSC99_IDENTIFIER_BYTES: usize = 128;
const MAX_PENDING_OSC99_NOTIFICATIONS: usize = 32;
const OSC99_PENDING_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum TerminalNotificationOccasion {
    #[default]
    Always,
    Unfocused,
    Invisible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalNotificationSource {
    Osc9,
    Osc99,
    Osc777,
    Bell,
}

impl TerminalNotificationSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Osc9 => "osc9",
            Self::Osc99 => "osc99",
            Self::Osc777 => "osc777",
            Self::Bell => "bell",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalNotification {
    pub(crate) title: Option<String>,
    pub(crate) body: Option<String>,
    pub(crate) occasion: TerminalNotificationOccasion,
    pub(crate) source: TerminalNotificationSource,
}

impl TerminalNotification {
    fn with_body(body: String, source: TerminalNotificationSource) -> Self {
        Self {
            title: None,
            body: Some(body),
            occasion: TerminalNotificationOccasion::Always,
            source,
        }
    }

    pub(crate) fn bell(body: String) -> Self {
        Self {
            title: None,
            body: Some(body),
            occasion: TerminalNotificationOccasion::Invisible,
            source: TerminalNotificationSource::Bell,
        }
    }
}

#[derive(Clone, Copy, Default)]
enum OscTerminalState {
    #[default]
    Ground,
    Escape,
    Command,
    Payload,
    PayloadEscape,
    Ignore,
    IgnoreEscape,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OscTerminalEvent {
    Notification(TerminalNotification),
    ProtocolReply(Vec<u8>),
    CommandStarted,
    CommandFinished,
}

#[derive(Debug)]
struct Osc99TextChunk {
    payload: String,
    base64_encoded: bool,
}

#[derive(Debug)]
struct PendingOsc99Notification {
    title_chunks: Vec<Osc99TextChunk>,
    body_chunks: Vec<Osc99TextChunk>,
    occasion: TerminalNotificationOccasion,
    encoded_bytes: usize,
    updated_at: Instant,
}

impl PendingOsc99Notification {
    fn new(updated_at: Instant) -> Self {
        Self {
            title_chunks: Vec::new(),
            body_chunks: Vec::new(),
            occasion: TerminalNotificationOccasion::Always,
            encoded_bytes: 0,
            updated_at,
        }
    }

    fn push(
        &mut self,
        payload_type: Osc99PayloadType,
        payload: String,
        base64_encoded: bool,
        occasion: Option<TerminalNotificationOccasion>,
        now: Instant,
    ) -> bool {
        self.encoded_bytes = self.encoded_bytes.saturating_add(payload.len());
        if self.encoded_bytes > MAX_NOTIFICATION_TEXT_BYTES {
            return false;
        }

        let chunks = match payload_type {
            Osc99PayloadType::Title => &mut self.title_chunks,
            Osc99PayloadType::Body => &mut self.body_chunks,
        };
        chunks.push(Osc99TextChunk {
            payload,
            base64_encoded,
        });
        if let Some(occasion) = occasion {
            self.occasion = occasion;
        }
        self.updated_at = now;
        true
    }

    fn touch(&mut self, occasion: Option<TerminalNotificationOccasion>, updated_at: Instant) {
        if let Some(occasion) = occasion {
            self.occasion = occasion;
        }
        self.updated_at = updated_at;
    }

    fn finish(self) -> Option<TerminalNotification> {
        let title = decode_osc99_text(&self.title_chunks)?;
        let body = decode_osc99_text(&self.body_chunks)?;
        let decoded_bytes =
            title.as_ref().map_or(0, String::len) + body.as_ref().map_or(0, String::len);
        if decoded_bytes > MAX_NOTIFICATION_TEXT_BYTES || (title.is_none() && body.is_none()) {
            return None;
        }

        Some(TerminalNotification {
            title,
            body,
            occasion: self.occasion,
            source: TerminalNotificationSource::Osc99,
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum Osc99PayloadType {
    Title,
    Body,
}

struct Osc99Metadata {
    identifier: Option<String>,
    payload_type: String,
    done: bool,
    base64_encoded: bool,
    occasion: Option<TerminalNotificationOccasion>,
}

#[derive(Default)]
struct OscTerminalParser {
    state: OscTerminalState,
    command: Vec<u8>,
    payload: Vec<u8>,
    pending_osc99: HashMap<String, PendingOsc99Notification>,
}

impl OscTerminalParser {
    /// Scans decoded terminal output without consuming it from the terminal emulator.
    fn advance(&mut self, bytes: &[u8]) -> Vec<OscTerminalEvent> {
        let mut events = Vec::new();

        for &byte in bytes {
            match self.state {
                OscTerminalState::Ground => {
                    if byte == 0x1b {
                        self.state = OscTerminalState::Escape;
                    }
                }
                OscTerminalState::Escape => {
                    self.state = match byte {
                        b']' => {
                            self.command.clear();
                            self.payload.clear();
                            OscTerminalState::Command
                        }
                        0x1b => OscTerminalState::Escape,
                        _ => OscTerminalState::Ground,
                    };
                }
                OscTerminalState::Command => match byte {
                    b';' if matches!(
                        self.command.as_slice(),
                        b"9" | b"99" | b"133" | b"633" | b"777"
                    ) =>
                    {
                        self.payload.clear();
                        self.state = OscTerminalState::Payload;
                    }
                    b';' => self.state = OscTerminalState::Ignore,
                    0x07 | 0x9c => self.reset(),
                    0x1b => self.state = OscTerminalState::IgnoreEscape,
                    b'0'..=b'9' if self.command.len() < 4 => self.command.push(byte),
                    _ => self.state = OscTerminalState::Ignore,
                },
                OscTerminalState::Payload => match byte {
                    0x07 | 0x9c => {
                        if let Some(event) = self.complete_event() {
                            events.push(event);
                        }
                    }
                    0x1b => self.state = OscTerminalState::PayloadEscape,
                    _ => self.push_payload_byte(byte),
                },
                OscTerminalState::PayloadEscape => {
                    if byte == b'\\' {
                        if let Some(event) = self.complete_event() {
                            events.push(event);
                        }
                    } else {
                        self.push_payload_byte(0x1b);
                        if matches!(self.state, OscTerminalState::Ignore) {
                            continue;
                        }
                        if byte == 0x1b {
                            self.state = OscTerminalState::PayloadEscape;
                        } else {
                            self.push_payload_byte(byte);
                        }
                    }
                }
                OscTerminalState::Ignore => match byte {
                    0x07 | 0x9c => self.reset(),
                    0x1b => self.state = OscTerminalState::IgnoreEscape,
                    _ => {}
                },
                OscTerminalState::IgnoreEscape => {
                    self.state = match byte {
                        b'\\' => {
                            self.command.clear();
                            self.payload.clear();
                            OscTerminalState::Ground
                        }
                        0x1b => OscTerminalState::IgnoreEscape,
                        _ => OscTerminalState::Ignore,
                    };
                }
            }
        }

        events
    }

    fn push_payload_byte(&mut self, byte: u8) {
        if self.payload.len() >= MAX_OSC_PAYLOAD_BYTES {
            self.payload.clear();
            self.state = OscTerminalState::Ignore;
        } else {
            self.payload.push(byte);
            self.state = OscTerminalState::Payload;
        }
    }

    fn complete_event(&mut self) -> Option<OscTerminalEvent> {
        let command = std::mem::take(&mut self.command);
        let payload = std::mem::take(&mut self.payload);
        self.reset();

        let payload = String::from_utf8_lossy(&payload);
        let trimmed = payload.trim();
        match command.as_slice() {
            b"9" => {
                // OSC 9 also namespaces Windows Terminal progress and CWD commands.
                if trimmed.is_empty()
                    || trimmed == "4"
                    || trimmed.starts_with("4;")
                    || trimmed.starts_with("9;")
                {
                    return None;
                }

                sanitize_notification_text(trimmed).map(|message| {
                    OscTerminalEvent::Notification(TerminalNotification::with_body(
                        message,
                        TerminalNotificationSource::Osc9,
                    ))
                })
            }
            b"99" => self.parse_osc99(trimmed),
            b"133" | b"633" => match trimmed.split(';').next() {
                Some("C") => Some(OscTerminalEvent::CommandStarted),
                Some("A" | "D") => Some(OscTerminalEvent::CommandFinished),
                _ => None,
            },
            b"777" => parse_osc777(trimmed).map(OscTerminalEvent::Notification),
            _ => None,
        }
    }

    fn parse_osc99(&mut self, payload: &str) -> Option<OscTerminalEvent> {
        let now = Instant::now();
        self.pending_osc99.retain(|_, pending| {
            now.saturating_duration_since(pending.updated_at) <= OSC99_PENDING_TTL
        });

        let (metadata, payload) = payload.split_once(';')?;
        let metadata = parse_osc99_metadata(metadata)?;
        if metadata.payload_type == "?" {
            let identifier = metadata.identifier?;
            return Some(OscTerminalEvent::ProtocolReply(
                format!(
                    "\x1b]99;i={identifier}:p=?;p=title,body:o=always,unfocused,invisible\x1b\\"
                )
                .into_bytes(),
            ));
        }

        let payload_type = match metadata.payload_type.as_str() {
            "title" => Some(Osc99PayloadType::Title),
            "body" => Some(Osc99PayloadType::Body),
            _ => None,
        };

        let identifier = metadata.identifier;
        let Some(payload_type) = payload_type else {
            let identifier = identifier?;
            let pending = self.pending_osc99.get_mut(&identifier)?;
            pending.touch(metadata.occasion, now);
            if !metadata.done {
                return None;
            }
            return self
                .pending_osc99
                .remove(&identifier)
                .and_then(PendingOsc99Notification::finish)
                .map(OscTerminalEvent::Notification);
        };

        if let Some(identifier) = identifier {
            if let Some(pending) = self.pending_osc99.get_mut(&identifier) {
                if !pending.push(
                    payload_type,
                    payload.to_string(),
                    metadata.base64_encoded,
                    metadata.occasion,
                    now,
                ) {
                    self.pending_osc99.remove(&identifier);
                    return None;
                }
                if !metadata.done {
                    return None;
                }

                return self
                    .pending_osc99
                    .remove(&identifier)
                    .and_then(PendingOsc99Notification::finish)
                    .map(OscTerminalEvent::Notification);
            }

            let mut pending = PendingOsc99Notification::new(now);
            if !pending.push(
                payload_type,
                payload.to_string(),
                metadata.base64_encoded,
                metadata.occasion,
                now,
            ) {
                return None;
            }
            if metadata.done {
                return pending.finish().map(OscTerminalEvent::Notification);
            }
            if self.pending_osc99.len() >= MAX_PENDING_OSC99_NOTIFICATIONS {
                return None;
            }

            self.pending_osc99.insert(identifier, pending);
            None
        } else {
            if !metadata.done {
                return None;
            }

            let mut pending = PendingOsc99Notification::new(now);
            pending
                .push(
                    payload_type,
                    payload.to_string(),
                    metadata.base64_encoded,
                    metadata.occasion,
                    now,
                )
                .then(|| pending.finish())
                .flatten()
                .map(OscTerminalEvent::Notification)
        }
    }

    fn reset(&mut self) {
        self.state = OscTerminalState::Ground;
        self.command.clear();
        self.payload.clear();
    }
}

fn parse_osc777(payload: &str) -> Option<TerminalNotification> {
    let mut parts = payload.splitn(3, ';');
    if parts.next()? != "notify" {
        return None;
    }

    let title = parts.next().and_then(sanitize_notification_text);
    let body = parts.next().and_then(sanitize_notification_text);
    if title.is_none() && body.is_none() {
        return None;
    }

    Some(TerminalNotification {
        title,
        body,
        occasion: TerminalNotificationOccasion::Always,
        source: TerminalNotificationSource::Osc777,
    })
}

fn parse_osc99_metadata(metadata: &str) -> Option<Osc99Metadata> {
    let mut parsed = Osc99Metadata {
        identifier: None,
        payload_type: "title".to_string(),
        done: true,
        base64_encoded: false,
        occasion: None,
    };

    for field in metadata.split(':').filter(|field| !field.is_empty()) {
        let (key, value) = field.split_once('=')?;
        if key.len() != 1 || !key.as_bytes()[0].is_ascii_alphabetic() {
            return None;
        }

        match key {
            "i" => {
                if !valid_osc99_identifier(value) {
                    return None;
                }
                parsed.identifier = Some(value.to_string());
            }
            "p" => parsed.payload_type = value.to_string(),
            "d" => parsed.done = parse_osc99_flag(value)?,
            "e" => parsed.base64_encoded = parse_osc99_flag(value)?,
            "o" => {
                parsed.occasion = Some(match value {
                    "always" => TerminalNotificationOccasion::Always,
                    "unfocused" => TerminalNotificationOccasion::Unfocused,
                    "invisible" => TerminalNotificationOccasion::Invisible,
                    _ => return None,
                });
            }
            _ => {}
        }
    }

    Some(parsed)
}

fn parse_osc99_flag(value: &str) -> Option<bool> {
    match value {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

fn valid_osc99_identifier(identifier: &str) -> bool {
    !identifier.is_empty()
        && identifier.len() <= MAX_OSC99_IDENTIFIER_BYTES
        && identifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'+' | b'.'))
}

fn decode_osc99_text(chunks: &[Osc99TextChunk]) -> Option<Option<String>> {
    if chunks.is_empty() {
        return Some(None);
    }

    let mut decoded = Vec::new();
    let mut index = 0;
    while index < chunks.len() {
        if !chunks[index].base64_encoded {
            decoded.extend_from_slice(chunks[index].payload.as_bytes());
            index += 1;
            continue;
        }

        let start = index;
        while index < chunks.len() && chunks[index].base64_encoded {
            index += 1;
        }
        let encoded_chunks = &chunks[start..index];
        let decoded_chunks = encoded_chunks
            .iter()
            .map(|chunk| decode_base64(&chunk.payload))
            .collect::<Option<Vec<_>>>();
        if let Some(decoded_chunks) = decoded_chunks {
            decoded.extend(decoded_chunks.into_iter().flatten());
        } else {
            let encoded = encoded_chunks
                .iter()
                .map(|chunk| chunk.payload.as_str())
                .collect::<String>();
            decoded.extend(decode_base64(&encoded)?);
        }
        if decoded.len() > MAX_NOTIFICATION_TEXT_BYTES {
            return None;
        }
    }

    let decoded = String::from_utf8(decoded).ok()?;
    Some(sanitize_notification_text(&decoded))
}

fn decode_base64(encoded: &str) -> Option<Vec<u8>> {
    general_purpose::STANDARD
        .decode(encoded)
        .or_else(|_| general_purpose::STANDARD_NO_PAD.decode(encoded))
        .ok()
}

fn sanitize_notification_text(text: &str) -> Option<String> {
    let sanitized = text
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'))
        .collect::<String>();
    let sanitized = sanitized.trim();
    (!sanitized.is_empty() && sanitized.len() <= MAX_NOTIFICATION_TEXT_BYTES)
        .then(|| sanitized.to_string())
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
    TerminalBell {
        tab_id: String,
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
    osc_terminal_parser: OscTerminalParser,
    output_activity_until: Option<Instant>,
    command_running: bool,
    shell_integration_available: bool,
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

#[cfg(test)]
mod osc_terminal_tests {
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };

    use super::{
        BackendCommand, BackendEvent, BackendTx, GuardedBackendEventSender, MAX_OSC_PAYLOAD_BYTES,
        MAX_PENDING_OSC99_NOTIFICATIONS, OSC99_PENDING_TTL, OscTerminalEvent, OscTerminalParser,
        TerminalNotification, TerminalNotificationOccasion, TerminalNotificationSource,
        TerminalTab,
    };

    fn notification(
        title: Option<&str>,
        body: Option<&str>,
        occasion: TerminalNotificationOccasion,
        source: TerminalNotificationSource,
    ) -> OscTerminalEvent {
        OscTerminalEvent::Notification(TerminalNotification {
            title: title.map(str::to_string),
            body: body.map(str::to_string),
            occasion,
            source,
        })
    }

    #[test]
    fn parses_bel_and_string_terminated_notifications() {
        let mut parser = OscTerminalParser::default();

        assert_eq!(
            parser.advance(b"before\x1b]9;build complete\x07after"),
            vec![notification(
                None,
                Some("build complete"),
                TerminalNotificationOccasion::Always,
                TerminalNotificationSource::Osc9,
            )]
        );
        assert_eq!(
            parser.advance(b"\x1b]9;deployment complete\x1b\\"),
            vec![notification(
                None,
                Some("deployment complete"),
                TerminalNotificationOccasion::Always,
                TerminalNotificationSource::Osc9,
            )]
        );
    }

    #[test]
    fn preserves_notifications_split_across_output_chunks() {
        let mut parser = OscTerminalParser::default();

        assert!(parser.advance(b"\x1b]9;task").is_empty());
        assert!(parser.advance(b" finished\x1b").is_empty());
        assert_eq!(
            parser.advance(b"\\"),
            vec![notification(
                None,
                Some("task finished"),
                TerminalNotificationOccasion::Always,
                TerminalNotificationSource::Osc9,
            )]
        );
    }

    #[test]
    fn parses_osc777_title_and_body() {
        let mut parser = OscTerminalParser::default();

        assert_eq!(
            parser.advance(b"\x1b]777;notify;Deploy;Production; ready\x1b\\"),
            vec![notification(
                Some("Deploy"),
                Some("Production; ready"),
                TerminalNotificationOccasion::Always,
                TerminalNotificationSource::Osc777,
            )]
        );
    }

    #[test]
    fn parses_simple_and_chunked_osc99_notifications() {
        let mut parser = OscTerminalParser::default();

        assert_eq!(
            parser.advance(b"\x1b]99;;Simple title\x1b\\"),
            vec![notification(
                Some("Simple title"),
                None,
                TerminalNotificationOccasion::Always,
                TerminalNotificationSource::Osc99,
            )]
        );
        assert!(
            parser
                .advance(b"\x1b]99;i=build-1:d=0:o=unfocused;Build finished\x1b\\")
                .is_empty()
        );
        assert_eq!(
            parser.advance(b"\x1b]99;i=build-1:p=body;Artifacts are ready\x1b\\"),
            vec![notification(
                Some("Build finished"),
                Some("Artifacts are ready"),
                TerminalNotificationOccasion::Unfocused,
                TerminalNotificationSource::Osc99,
            )]
        );
    }

    #[test]
    fn ignores_unsupported_osc99_payloads_without_losing_supported_text() {
        let mut parser = OscTerminalParser::default();

        assert!(
            parser
                .advance(b"\x1b]99;i=rich-1:d=0;Rich notification\x1b\\")
                .is_empty()
        );
        assert_eq!(
            parser.advance(b"\x1b]99;i=rich-1:p=buttons;Open\x1b\\"),
            vec![notification(
                Some("Rich notification"),
                None,
                TerminalNotificationOccasion::Always,
                TerminalNotificationSource::Osc99,
            )]
        );
    }

    #[test]
    fn decodes_both_osc99_base64_chunking_modes() {
        let mut parser = OscTerminalParser::default();

        assert!(
            parser
                .advance(b"\x1b]99;i=encoded-1:d=0:e=1;VGFzaw==\x1b\\")
                .is_empty()
        );
        assert_eq!(
            parser.advance(b"\x1b]99;i=encoded-1:e=1;IGRvbmU=\x1b\\"),
            vec![notification(
                Some("Task done"),
                None,
                TerminalNotificationOccasion::Always,
                TerminalNotificationSource::Osc99,
            )]
        );

        assert!(
            parser
                .advance(b"\x1b]99;i=encoded-2:d=0:e=1;RGV\x1b\\")
                .is_empty()
        );
        assert_eq!(
            parser.advance(b"\x1b]99;i=encoded-2:e=1;wbG95ZWQ=\x1b\\"),
            vec![notification(
                Some("Deployed"),
                None,
                TerminalNotificationOccasion::Always,
                TerminalNotificationSource::Osc99,
            )]
        );
    }

    #[test]
    fn replies_to_osc99_capability_queries_and_rejects_unsafe_ids() {
        let mut parser = OscTerminalParser::default();

        assert_eq!(
            parser.advance(b"\x1b]99;i=query-1:p=?;\x1b\\"),
            vec![OscTerminalEvent::ProtocolReply(
                b"\x1b]99;i=query-1:p=?;p=title,body:o=always,unfocused,invisible\x1b\\".to_vec(),
            )]
        );
        assert!(parser.advance(b"\x1b]99;i=unsafe/id:p=?;\x1b\\").is_empty());
    }

    #[test]
    fn sends_osc99_capability_replies_to_the_terminal_backend() {
        let (backend_tx, backend_rx) = mpsc::channel();
        let (events_tx, _events_rx) = mpsc::channel();
        let mut tab = TerminalTab::new_local(
            "tab-1".into(),
            "Local".into(),
            BackendTx::Local(backend_tx),
            GuardedBackendEventSender::new(events_tx),
        );

        assert!(tab.feed(b"\x1b]99;i=query-2:p=?;\x1b\\").is_empty());
        assert!(matches!(
            backend_rx.try_recv(),
            Ok(BackendCommand::Input(reply))
                if reply.as_slice()
                    == b"\x1b]99;i=query-2:p=?;p=title,body:o=always,unfocused,invisible\x1b\\"
        ));
    }

    #[test]
    fn bounds_and_expires_pending_osc99_notifications() {
        let mut parser = OscTerminalParser::default();
        for index in 0..MAX_PENDING_OSC99_NOTIFICATIONS {
            let sequence = format!("\x1b]99;i=pending-{index}:d=0;title\x1b\\");
            assert!(parser.advance(sequence.as_bytes()).is_empty());
        }
        assert_eq!(parser.pending_osc99.len(), MAX_PENDING_OSC99_NOTIFICATIONS);

        assert!(
            parser
                .advance(b"\x1b]99;i=overflow:d=0;ignored\x1b\\")
                .is_empty()
        );
        assert!(!parser.pending_osc99.contains_key("overflow"));

        parser
            .pending_osc99
            .get_mut("pending-0")
            .unwrap()
            .updated_at = Instant::now() - OSC99_PENDING_TTL - Duration::from_secs(1);
        assert_eq!(parser.advance(b"\x1b]99;;cleanup\x1b\\").len(), 1);
        assert!(!parser.pending_osc99.contains_key("pending-0"));
    }

    #[test]
    fn parses_shell_integration_command_lifecycle_events() {
        let mut parser = OscTerminalParser::default();

        assert_eq!(
            parser.advance(b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07"),
            vec![
                OscTerminalEvent::CommandFinished,
                OscTerminalEvent::CommandStarted,
            ]
        );
        assert_eq!(
            parser.advance(b"\x1b]133;D;0\x1b\\"),
            vec![OscTerminalEvent::CommandFinished]
        );
        assert_eq!(
            parser.advance(b"\x1b]633;C\x07\x1b]633;D;0\x07"),
            vec![
                OscTerminalEvent::CommandStarted,
                OscTerminalEvent::CommandFinished,
            ]
        );
    }

    #[test]
    fn ignores_other_osc_commands_and_windows_terminal_namespaces() {
        let mut parser = OscTerminalParser::default();

        assert!(parser.advance(b"\x1b]2;window title\x07").is_empty());
        assert!(parser.advance(b"\x1b]9;4\x07").is_empty());
        assert!(parser.advance(b"\x1b]9;4;1;50\x07").is_empty());
        assert!(parser.advance(b"\x1b]9;9;C:\\workspace\x07").is_empty());
    }

    #[test]
    fn drops_oversized_notification_payloads_and_recovers() {
        let mut parser = OscTerminalParser::default();
        let mut oversized = b"\x1b]9;".to_vec();
        oversized.extend(std::iter::repeat_n(b'x', MAX_OSC_PAYLOAD_BYTES + 1));
        oversized.push(0x07);

        assert!(parser.advance(&oversized).is_empty());
        assert_eq!(
            parser.advance(b"\x1b]9;next task\x07"),
            vec![notification(
                None,
                Some("next task"),
                TerminalNotificationOccasion::Always,
                TerminalNotificationSource::Osc9,
            )]
        );
    }

    #[test]
    fn forwards_standalone_bell_without_duplicating_osc_terminator() {
        let (events_tx, events_rx) = mpsc::channel();
        let mut tab = TerminalTab::new_local(
            "tab-1".into(),
            "Local".into(),
            BackendTx::Pending,
            GuardedBackendEventSender::new(events_tx),
        );

        assert_eq!(
            tab.feed(b"\x1b]9;task complete\x07"),
            vec![TerminalNotification {
                title: None,
                body: Some("task complete".to_string()),
                occasion: TerminalNotificationOccasion::Always,
                source: TerminalNotificationSource::Osc9,
            }]
        );
        assert!(events_rx.try_recv().is_err());

        assert_eq!(
            tab.feed(b"\x1b]99;;second task\x07"),
            vec![TerminalNotification {
                title: Some("second task".to_string()),
                body: None,
                occasion: TerminalNotificationOccasion::Always,
                source: TerminalNotificationSource::Osc99,
            }]
        );
        assert!(events_rx.try_recv().is_err());

        assert!(tab.feed(b"\x07").is_empty());
        assert!(matches!(
            events_rx.try_recv(),
            Ok(BackendEvent::TerminalBell { tab_id }) if tab_id == "tab-1"
        ));
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
            osc_terminal_parser: OscTerminalParser::default(),
            output_activity_until: None,
            command_running: false,
            shell_integration_available: false,
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

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<TerminalNotification> {
        let decoded = self.output_decoder.decode(bytes);
        if !decoded.is_empty() {
            self.output_activity_until = Some(Instant::now() + TERMINAL_ACTIVITY_GRACE);
        }
        let mut notifications = Vec::new();
        for event in self.osc_terminal_parser.advance(&decoded) {
            match event {
                OscTerminalEvent::Notification(notification) => {
                    self.command_running = false;
                    self.output_activity_until = None;
                    notifications.push(notification);
                }
                OscTerminalEvent::ProtocolReply(reply) => {
                    self.send_backend(BackendCommand::Input(reply));
                }
                OscTerminalEvent::CommandStarted => {
                    self.shell_integration_available = true;
                    self.command_running = true;
                }
                OscTerminalEvent::CommandFinished => {
                    self.shell_integration_available = true;
                    self.command_running = false;
                    self.output_activity_until = None;
                }
            }
        }
        self.processor.advance(&mut self.term, &decoded);
        notifications
    }

    pub(crate) fn is_command_active(&self) -> bool {
        (self.command_running && !self.is_alternate_screen_active())
            || self.output_activity_until.is_some()
    }

    pub(crate) fn expire_output_activity(&mut self, now: Instant) -> bool {
        if self
            .output_activity_until
            .is_some_and(|deadline| now >= deadline)
        {
            self.output_activity_until = None;
            true
        } else {
            false
        }
    }

    pub(crate) fn record_terminal_input(&mut self, bytes: &[u8]) {
        if self.shell_integration_available
            && !self.is_alternate_screen_active()
            && bytes.iter().any(|byte| matches!(byte, b'\r' | b'\n'))
        {
            self.command_running = true;
            self.output_activity_until = None;
        }
    }

    pub(crate) fn clear_command_activity(&mut self) -> bool {
        let changed = self.command_running || self.output_activity_until.is_some();
        self.command_running = false;
        self.output_activity_until = None;
        self.shell_integration_available = false;
        self.osc_terminal_parser = OscTerminalParser::default();
        changed
    }

    pub(crate) fn report_focus(&self, focused: bool) {
        if self.term.mode().contains(TermMode::FOCUS_IN_OUT) {
            self.send_backend(BackendCommand::Input(if focused {
                b"\x1b[I".to_vec()
            } else {
                b"\x1b[O".to_vec()
            }));
        }
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
        self.osc_terminal_parser = OscTerminalParser::default();
        self.output_activity_until = None;
        self.command_running = false;
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

    pub fn resize(&mut self, cols: u16, rows: u16) -> bool {
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
            true
        } else {
            false
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
            Event::Bell => {
                let _ = self.events.send(BackendEvent::TerminalBell {
                    tab_id: self.tab_id.clone(),
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

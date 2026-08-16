pub mod config_sync;
pub mod constants;
pub mod controls;
pub mod dialogs;
pub mod keybinding_recorder;
pub mod resizable;
pub mod search;
pub mod startup;
pub mod system_menu;
pub mod theme;
pub mod ui;

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    ops::Range,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};

use crate::app::resizable::ResizableState;
use gpui::{
    AppContext as _, Bounds, Context, Entity, FocusHandle, Pixels, Point, SharedString, Size,
    UniformListScrollHandle, Window, point, px, size,
};
use gpui_component::{
    Theme, ThemeMode, ThemeRegistry,
    input::{InputEvent, InputState},
    scroll::ScrollbarHandle,
};
use rust_i18n::t;
use tokio::runtime::Runtime;

use crate::{
    session::config::{AuthMethod, ConfigStore},
    session::ssh_config::SshConfigEntry,
    system::{RemotePort, RemoteProcess, SystemSampler, SystemSnapshot},
    terminal::{
        self, BackendEvent, TabKind, TerminalNotification, TerminalNotificationOccasion,
        TerminalTab,
    },
    text_encoding::TextEncoding,
};

#[derive(Clone, Debug)]
pub(crate) enum PaneLayout {
    Single(String),
    Horizontal(Vec<PaneLayout>, f32), // children, split_ratio (0.0-1.0)
    Vertical(Vec<PaneLayout>, f32),   // children, split_ratio (0.0-1.0)
}

#[derive(Clone)]
pub(crate) struct TabGroup {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) pane_root: PaneLayout,
    pub(crate) sftp: Option<crate::terminal::SftpUiState>,
    pub(crate) sftp_tab_id: Option<String>,
}

impl PaneLayout {
    pub fn tab_ids(&self) -> Vec<&str> {
        match self {
            PaneLayout::Single(id) => vec![id.as_str()],
            PaneLayout::Horizontal(children, _) | PaneLayout::Vertical(children, _) => {
                children.iter().flat_map(|c| c.tab_ids()).collect()
            }
        }
    }

    pub fn contains(&self, tab_id: &str) -> bool {
        match self {
            PaneLayout::Single(id) => id == tab_id,
            PaneLayout::Horizontal(children, _) | PaneLayout::Vertical(children, _) => {
                children.iter().any(|c| c.contains(tab_id))
            }
        }
    }

    pub fn focused_tab_id(&self, path: &[usize]) -> Option<&str> {
        match self {
            PaneLayout::Single(id) if path.is_empty() => Some(id.as_str()),
            PaneLayout::Horizontal(children, _) | PaneLayout::Vertical(children, _) => {
                let (&first, rest) = path.split_first()?;
                children.get(first).and_then(|c| c.focused_tab_id(rest))
            }
            _ => None,
        }
    }

    pub fn replace_at(&mut self, path: &[usize], replacement: PaneLayout) {
        match (self, path) {
            (this @ PaneLayout::Single(_), []) => *this = replacement,
            (
                PaneLayout::Horizontal(children, _) | PaneLayout::Vertical(children, _),
                [first, rest @ ..],
            ) => {
                if let Some(child) = children.get_mut(*first) {
                    child.replace_at(rest, replacement);
                }
            }
            _ => {}
        }
    }

    pub fn remove_tab(&mut self, tab_id: &str) -> bool {
        match self {
            PaneLayout::Single(id) if id == tab_id => {
                *self = PaneLayout::Single(String::new());
                true
            }
            PaneLayout::Single(_) => false,
            PaneLayout::Horizontal(children, _) | PaneLayout::Vertical(children, _) => {
                for child in children.iter_mut() {
                    child.remove_tab(tab_id);
                }
                children.retain(|c| !matches!(c, PaneLayout::Single(id) if id.is_empty()));
                if children.is_empty() {
                    *self = PaneLayout::Single(String::new());
                } else if children.len() == 1 {
                    if let Some(replacement) = children.pop() {
                        *self = replacement;
                    }
                }
                true
            }
        }
    }

    #[allow(dead_code)]
    pub fn total_panes(&self) -> usize {
        match self {
            PaneLayout::Single(_) => 1,
            PaneLayout::Horizontal(children, _) | PaneLayout::Vertical(children, _) => {
                children.iter().map(|c| c.total_panes()).sum()
            }
        }
    }
}

fn should_show_terminal_notification(
    occasion: TerminalNotificationOccasion,
    window_active: bool,
    terminal_visible: bool,
) -> bool {
    match occasion {
        TerminalNotificationOccasion::Always => true,
        TerminalNotificationOccasion::Unfocused => !window_active,
        TerminalNotificationOccasion::Invisible => !terminal_visible,
    }
}

pub(crate) struct TerminalScrollbarState {
    line_height: Pixels,
    total_lines: usize,
    viewport_lines: usize,
    display_offset: usize,
}

#[derive(Clone, Default)]
pub(crate) struct TerminalScrollbarHandle {
    state: Rc<RefCell<Option<TerminalScrollbarState>>>,
    pub(crate) future_display_offset: Rc<Cell<Option<usize>>>,
}

impl TerminalScrollbarHandle {
    pub(crate) fn update(&self, snapshot: &terminal::RenderSnapshot, line_height: Pixels) {
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
        let state_ref = self.state.borrow();
        let Some(state) = state_ref.as_ref() else {
            return point(px(0.), px(0.));
        };
        let scroll_offset = state
            .total_lines
            .saturating_sub(state.viewport_lines)
            .saturating_sub(state.display_offset);
        point(px(0.), -(scroll_offset as f32 * state.line_height))
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        let state_ref = self.state.borrow();
        let Some(state) = state_ref.as_ref() else {
            return;
        };
        let offset_delta = (offset.y / state.line_height).round() as i32;
        let max_offset = state.total_lines.saturating_sub(state.viewport_lines);
        let display_offset = (max_offset as i32 + offset_delta).clamp(0, max_offset as i32);
        self.future_display_offset
            .set(Some(display_offset as usize));
    }

    fn content_size(&self) -> Size<Pixels> {
        let state_ref = self.state.borrow();
        let Some(state) = state_ref.as_ref() else {
            return size(px(0.), px(0.));
        };
        size(
            px(0.),
            state.total_lines.max(state.viewport_lines) as f32 * state.line_height,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DialogKind {
    About,
    Settings,
    SessionSelector,
    ConnectionExport,
    Transfers,
    NewSsh,
    ConnectionGroup,
    SftpRename,
    SftpEditor,
    Processes,
    Ports,
    SshReconnect,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ServerMonitorView {
    #[default]
    Cpu,
    Memory,
}

pub(crate) struct Ashell {
    pub(crate) focus_handle: FocusHandle,
    pub(crate) selector_focus_handle: FocusHandle,
    pub(crate) host_input: Entity<InputState>,
    pub(crate) session_name_input: Entity<InputState>,
    pub(crate) session_group: String,
    pub(crate) port_input: Entity<InputState>,
    pub(crate) user_input: Entity<InputState>,
    pub(crate) password_input: Entity<InputState>,
    pub(crate) key_path_input: Entity<InputState>,
    pub(crate) key_inline_input: Entity<InputState>,
    pub(crate) passphrase_input: Entity<InputState>,
    pub(crate) baud_rate_input: Entity<InputState>,
    pub(crate) session_protocol: String,
    pub(crate) ssh_terminal_encoding: TextEncoding,
    pub(crate) ssh_proxy_type: String,
    pub(crate) proxy_host_input: Entity<InputState>,
    pub(crate) proxy_port_input: Entity<InputState>,
    pub(crate) proxy_user_input: Entity<InputState>,
    pub(crate) proxy_password_input: Entity<InputState>,
    pub(crate) global_proxy_type: String,
    pub(crate) global_proxy_host_input: Entity<InputState>,
    pub(crate) global_proxy_port_input: Entity<InputState>,
    pub(crate) global_proxy_user_input: Entity<InputState>,
    pub(crate) global_proxy_password_input: Entity<InputState>,
    pub(crate) sync_endpoint_input: Entity<InputState>,
    pub(crate) sync_username_input: Entity<InputState>,
    pub(crate) sync_webdav_password_input: Entity<InputState>,
    pub(crate) sync_s3_endpoint_input: Entity<InputState>,
    pub(crate) sync_s3_region_input: Entity<InputState>,
    pub(crate) sync_s3_bucket_input: Entity<InputState>,
    pub(crate) sync_s3_object_key_input: Entity<InputState>,
    pub(crate) sync_s3_access_key_input: Entity<InputState>,
    pub(crate) sync_s3_secret_key_input: Entity<InputState>,
    pub(crate) sync_s3_session_token_input: Entity<InputState>,
    pub(crate) sync_encryption_password_input: Entity<InputState>,
    pub(crate) sync_in_progress: bool,
    pub(crate) sync_status: SharedString,
    pub(crate) sftp_path_input: Entity<InputState>,
    pub(crate) remote_process_filter_input: Entity<InputState>,
    pub(crate) remote_port_filter_input: Entity<InputState>,
    pub(crate) connection_filter_input: Entity<InputState>,
    pub(crate) connection_group_name_input: Entity<InputState>,
    pub(crate) command_history_filter_input: Entity<InputState>,
    pub(crate) selected_connection_ids: HashSet<String>,
    pub(crate) selected_command_history: HashSet<(String, usize)>,
    pub(crate) ssh_auth_method: AuthMethod,
    pub(crate) ssh_config_entries: Vec<SshConfigEntry>,
    pub(crate) ssh_config_selected: Option<usize>,
    pub(crate) editing_session_id: Option<String>,
    pub(crate) editing_connection_group: Option<String>,
    pub(crate) follow_system_theme: bool,
    pub(crate) theme_mode: ThemeMode,
    pub(crate) light_theme_name: SharedString,
    pub(crate) dark_theme_name: SharedString,
    pub(crate) ui_font_size: f32,
    pub(crate) terminal_font_size: f32,
    pub(crate) terminal_zoom_accumulator: f32,
    pub(crate) ui_font_family: SharedString,
    pub(crate) terminal_font_family: SharedString,
    pub(crate) tabs: Vec<TerminalTab>,
    pub(crate) active_tab: Option<String>,
    pub(crate) tab_groups: Vec<TabGroup>,
    pub(crate) active_group: Option<String>,
    pub(crate) selector_selection: usize,
    pub(crate) workspace_panels: Entity<ResizableState>,
    pub(crate) body_panels: Entity<ResizableState>,
    pub(crate) sftp_tree_panels: Entity<ResizableState>,
    pub(crate) sftp_file_columns: Entity<ResizableState>,
    pub(crate) is_layout_reset: bool,
    pub(crate) terminal_scrollbars: HashMap<String, TerminalScrollbarHandle>,
    pub(crate) remote_files_scroll_handle: UniformListScrollHandle,
    pub(crate) remote_files_horizontal_scroll_handle: gpui::ScrollHandle,
    pub(crate) remote_files_columns_viewport_width: Option<Pixels>,
    pub(crate) remote_tree_scroll_handle: UniformListScrollHandle,
    pub(crate) disk_scroll_handle: gpui::ScrollHandle,
    pub(crate) process_scroll_handle: gpui::ScrollHandle,
    pub(crate) port_scroll_handle: UniformListScrollHandle,
    pub(crate) tabs_scroll_handle: gpui::ScrollHandle,
    pub(crate) selector_scroll_handle: gpui::ScrollHandle,
    pub(crate) saved_scroll_handle: gpui::ScrollHandle,
    pub(crate) command_history_scroll_handle: gpui::ScrollHandle,
    pub(crate) connection_scroll_handle: gpui::ScrollHandle,
    pub(crate) saved_sessions_overflowing: bool,
    pub(crate) connection_progress: Option<ConnectionProgress>,
    pub(crate) pending_sftp_path_sync: Option<String>,
    pub(crate) sftp_context_menu: Option<SftpContextMenuState>,
    pub(crate) sftp_rename_input: Entity<InputState>,
    pub(crate) sftp_rename_state: Option<SftpRenameState>,
    pub(crate) sftp_editor_input: Entity<InputState>,
    pub(crate) sftp_editor_state: Option<SftpEditorState>,
    pub(crate) sftp_creating_folder: bool,
    pub(crate) sftp_new_folder_input: Entity<InputState>,
    pub(crate) sftp_delete_scroll_handle: gpui::ScrollHandle,
    pub(crate) show_hidden_files: bool,
    pub(crate) transfers: Vec<crate::terminal::Transfer>,
    pub(crate) show_transfers_dialog: bool,
    pub(crate) show_command_history: bool,
    pub(crate) ssh_command_buffers: HashMap<String, String>,
    pub(crate) ssh_command_starts: HashMap<String, (usize, usize)>,
    pub(crate) system_status: Option<SharedString>,
    pub(crate) server_monitor_view: ServerMonitorView,
    pub(crate) remote_processes: Vec<RemoteProcess>,
    pub(crate) remote_process_status: Option<SharedString>,
    pub(crate) remote_processes_in_flight: bool,
    pub(crate) expanded_process_pid: Option<u32>,
    pub(crate) remote_ports: Vec<RemotePort>,
    pub(crate) remote_ports_status: Option<SharedString>,
    pub(crate) remote_ports_in_flight: bool,
    pub(crate) terminating_processes: HashSet<u32>,
    pub(crate) pane_root: PaneLayout,
    pub(crate) focused_pane_path: Vec<usize>,
    pub(crate) terminal_panel_bounds: Option<Bounds<Pixels>>,
    pub(crate) terminal_bounds: HashMap<String, Bounds<Pixels>>,
    pub(crate) terminal_selecting: bool,
    pub(crate) dragging_splitter: Option<(Vec<usize>, usize)>, // (parent_path, child_index)
    pub(crate) drag_split_origin: Option<gpui::Point<Pixels>>,
    pub(crate) terminal_marked_text: Option<String>,
    pub(crate) sftp_panel_minimized: bool,
    pub(crate) sidebar_collapsed: bool,
    pub(crate) window_active: bool,
    pub(crate) native_window_handle: Option<isize>,
    pub(crate) unread_terminal_notifications: HashSet<String>,
    pub(crate) prev_monitoring_size: Option<Pixels>,
    pub(crate) status: SharedString,
    pub(crate) config: ConfigStore,
    pub(crate) active_title_bar_style: crate::session::config::TitleBarStyle,
    pub(crate) cursor_style: crate::session::config::CursorStyle,
    pub(crate) system_sampler: SystemSampler,
    pub(crate) recording_action: Option<String>,
    pub(crate) active_dialog: Option<DialogKind>,
    /// Error message when a recorded keybinding conflicts with another
    pub(crate) keybind_error: Option<(String, String)>, // (action_id, error_message)
    /// Whether workspace keybindings are currently suspended (during settings)
    pub(crate) keybinds_suspended: bool,
    pub(crate) system: SystemSnapshot,
    pub(crate) cpu_history: Vec<f32>,
    pub(crate) net_rx_history: Vec<f32>,
    pub(crate) net_tx_history: Vec<f32>,
    pub(crate) last_system_sample: Instant,
    pub(crate) last_theme_sync: Instant,

    pub(crate) search_input: Entity<InputState>,
    pub(crate) search_active: bool,
    pub(crate) search_query: String,
    pub(crate) search_matches: Vec<(i32, i32)>,
    pub(crate) search_current: usize,
    pub(crate) search_target_tab: Option<String>,
    pub(crate) search_bar_bounds: Option<Bounds<Pixels>>,

    pub(crate) system_tab_id: Option<String>,
    pub(crate) sftp_handles: std::collections::HashMap<String, crate::sftp::SftpHandle>,

    pub(crate) remote_sample_in_flight: bool,
    pub(crate) runtime: Runtime,
    pub(crate) events_rx: mpsc::Receiver<BackendEvent>,
    pub(crate) events_tx: mpsc::Sender<BackendEvent>,
    pub(crate) last_window_size: Option<gpui::Size<Pixels>>,
    pub(crate) last_sidebar_width: Option<Pixels>,
    pub(crate) pending_local_terminal_resizes: HashMap<String, (u16, u16)>,
    pub(crate) local_terminal_resize_task: Option<gpui::Task<()>>,
    pub(crate) hovered_url: Option<HoveredUrl>,
    pub(crate) cmd_ctrl_pressed: bool,
    pub(crate) _subscriptions: Vec<gpui::Subscription>,
    pub(crate) last_window_bounds: Option<gpui::WindowBounds>,
    pub(crate) window_bounds_save_task: Option<gpui::Task<()>>,
    pub(crate) save_lock: std::sync::Arc<std::sync::Mutex<()>>,
    pub(crate) save_latest_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HoveredUrl {
    pub(crate) url: String,
    pub(crate) tab_id: String,
    pub(crate) cells: Vec<(usize, usize)>,
}

#[derive(Clone)]
pub(crate) enum SelectorEntry {
    Local,
    NewSsh,
    Saved(String),
}

#[derive(Clone)]
pub(crate) struct ConnectionProgress {
    pub(crate) tab_id: String,
    pub(crate) title: SharedString,
    pub(crate) lines: Vec<SharedString>,
    pub(crate) failed: bool,
}

#[derive(Clone)]
pub(crate) struct SftpContextMenuState {
    pub(crate) remote_path: String,
    pub(crate) is_dir: bool,
    pub(crate) position: Point<Pixels>,
}

#[derive(Clone, Debug)]
pub(crate) struct SftpRenameState {
    pub(crate) group_id: String,
    pub(crate) old_path: String,
    pub(crate) in_flight: bool,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) enum SftpEditorInteraction {
    Move {
        pointer_origin: Point<Pixels>,
        initial_bounds: Bounds<Pixels>,
    },
    Resize {
        pointer_origin: Point<Pixels>,
        initial_bounds: Bounds<Pixels>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct SftpEditorState {
    pub(crate) group_id: String,
    pub(crate) remote_path: String,
    pub(crate) raw_content: Vec<u8>,
    pub(crate) original_content: String,
    pub(crate) encoding: TextEncoding,
    pub(crate) has_bom: bool,
    pub(crate) decode_had_errors: bool,
    pub(crate) loaded: bool,
    pub(crate) loading: bool,
    pub(crate) saving: bool,
    pub(crate) message: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) bounds: Bounds<Pixels>,
    pub(crate) interaction: Option<SftpEditorInteraction>,
}

impl Ashell {
    fn transfer_source_title(&self, tab_id: &str) -> String {
        self.tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .map(|tab| tab.title.clone())
            .or_else(|| {
                self.tab_groups
                    .iter()
                    .find(|group| group.id == tab_id)
                    .map(|group| group.title.clone())
            })
            .or_else(|| {
                self.tab_groups
                    .iter()
                    .find(|group| group.pane_root.contains(tab_id))
                    .map(|group| group.title.clone())
            })
            .unwrap_or_else(|| "Unknown".to_string())
    }

    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
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
        let passphrase_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("SSH private key passphrase (optional)")
                .masked(true)
        });
        let baud_rate_input = cx.new(|cx| InputState::new(window, cx).default_value("115200"));
        let proxy_host_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("proxy_host").to_string()));
        let proxy_port_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("proxy_port").to_string()));
        let proxy_user_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("proxy_user").to_string()));
        let proxy_password_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("proxy_password").to_string())
                .masked(true)
        });
        let sftp_path_input = cx.new(|cx| InputState::new(window, cx).default_value("/"));
        let sftp_new_folder_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("new_folder").to_string()));
        let sftp_rename_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("new_name").to_string()));
        let sftp_editor_input = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor("text")
                .line_number(true)
                .soft_wrap(false)
        });
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("search").to_string()));
        let remote_process_filter_input = cx
            .new(|cx| InputState::new(window, cx).placeholder(t!("filter_processes").to_string()));
        let remote_port_filter_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("filter_ports").to_string()));
        let connection_filter_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("filter_connections").to_string())
        });
        let connection_group_name_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("connection_group_name").to_string())
        });
        let command_history_filter_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("filter_command_history").to_string())
        });
        let config = ConfigStore::load().unwrap_or_else(|err| {
            tracing::warn!("failed to load config: {err:#}");
            ConfigStore::in_memory()
        });
        let global_proxy_host_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("proxy_host").to_string())
                .default_value(config.global_proxy_host())
        });
        let global_proxy_port_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("proxy_port").to_string())
                .default_value(
                    config
                        .global_proxy_port()
                        .map(|p| p.to_string())
                        .unwrap_or_default(),
                )
        });
        let global_proxy_user_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("proxy_user").to_string())
                .default_value(config.global_proxy_user())
        });
        let global_proxy_password_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("proxy_password").to_string())
                .masked(true)
                .default_value(config.global_proxy_password())
        });
        let sync_endpoint_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://dav.example.com/ashell/")
                .default_value(config.sync_endpoint())
        });
        let sync_username_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("sync_username").to_string())
                .default_value(config.sync_username())
        });
        let sync_webdav_password_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("sync_webdav_password").to_string())
                .masked(true)
        });
        let sync_s3_endpoint_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://s3.example.com")
                .default_value(config.sync_s3_endpoint())
        });
        let sync_s3_region_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("us-east-1")
                .default_value(config.sync_s3_region())
        });
        let sync_s3_bucket_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("sync_s3_bucket").to_string())
                .default_value(config.sync_s3_bucket())
        });
        let sync_s3_object_key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("ashell-sync.json")
                .default_value(config.sync_s3_object_key())
        });
        let sync_s3_access_key_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("sync_s3_access_key").to_string())
        });
        let sync_s3_secret_key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("sync_s3_secret_key").to_string())
                .masked(true)
        });
        let sync_s3_session_token_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("sync_s3_session_token").to_string())
                .masked(true)
        });
        let sync_encryption_password_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("sync_encryption_password").to_string())
                .masked(true)
        });

        let _subscriptions = vec![
            cx.subscribe_in(&host_input, window, Self::on_input_event),
            cx.subscribe_in(&session_name_input, window, Self::on_input_event),
            cx.subscribe_in(&port_input, window, Self::on_input_event),
            cx.subscribe_in(&user_input, window, Self::on_input_event),
            cx.subscribe_in(&password_input, window, Self::on_input_event),
            cx.subscribe_in(&key_path_input, window, Self::on_input_event),
            cx.subscribe_in(&key_inline_input, window, Self::on_input_event),
            cx.subscribe_in(&passphrase_input, window, Self::on_input_event),
            cx.subscribe_in(&baud_rate_input, window, Self::on_input_event),
            cx.subscribe_in(&proxy_host_input, window, Self::on_input_event),
            cx.subscribe_in(&proxy_port_input, window, Self::on_input_event),
            cx.subscribe_in(&proxy_user_input, window, Self::on_input_event),
            cx.subscribe_in(&proxy_password_input, window, Self::on_input_event),
            cx.subscribe_in(&sftp_path_input, window, Self::on_input_event),
            cx.subscribe_in(&sftp_new_folder_input, window, Self::on_input_event),
            cx.subscribe_in(&sftp_rename_input, window, Self::on_input_event),
            cx.subscribe_in(&sftp_editor_input, window, Self::on_input_event),
            cx.subscribe_in(&search_input, window, Self::on_input_event),
            cx.subscribe_in(&remote_process_filter_input, window, Self::on_input_event),
            cx.subscribe_in(&remote_port_filter_input, window, Self::on_input_event),
            cx.subscribe_in(&connection_filter_input, window, Self::on_input_event),
            cx.subscribe_in(&connection_group_name_input, window, Self::on_input_event),
            cx.subscribe_in(&command_history_filter_input, window, Self::on_input_event),
            cx.subscribe_in(&sync_endpoint_input, window, Self::on_input_event),
            cx.subscribe_in(&sync_username_input, window, Self::on_input_event),
            cx.subscribe_in(&sync_webdav_password_input, window, Self::on_input_event),
            cx.subscribe_in(&sync_s3_endpoint_input, window, Self::on_input_event),
            cx.subscribe_in(&sync_s3_region_input, window, Self::on_input_event),
            cx.subscribe_in(&sync_s3_bucket_input, window, Self::on_input_event),
            cx.subscribe_in(&sync_s3_object_key_input, window, Self::on_input_event),
            cx.subscribe_in(&sync_s3_access_key_input, window, Self::on_input_event),
            cx.subscribe_in(&sync_s3_secret_key_input, window, Self::on_input_event),
            cx.subscribe_in(&sync_s3_session_token_input, window, Self::on_input_event),
            cx.subscribe_in(
                &sync_encryption_password_input,
                window,
                Self::on_input_event,
            ),
            cx.observe_window_activation(window, Self::on_window_activation_changed),
            cx.observe_window_bounds(window, Self::on_window_bounds_changed),
            cx.on_app_quit(Self::save_layout_on_app_quit),
        ];

        let (events_tx, events_rx) = mpsc::channel();
        let workspace_panels = cx.new(|_| ResizableState::default());
        let body_panels = cx.new(|_| ResizableState::default());
        let sftp_tree_panels = cx.new(|_| ResizableState::default());
        let sftp_file_columns = cx.new(|_| ResizableState::default());
        let mut system_sampler = SystemSampler::new();
        let system = system_sampler.sample();
        let default_light_theme_name = ThemeRegistry::global(cx).default_light_theme().name.clone();
        let default_dark_theme_name = ThemeRegistry::global(cx).default_dark_theme().name.clone();
        let follow_system_theme = config.follow_system_theme();

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
        connection_filter_input.update(cx, |input, cx| {
            input.set_placeholder(t!("filter_connections").to_string(), window, cx);
        });
        connection_group_name_input.update(cx, |input, cx| {
            input.set_placeholder(t!("connection_group_name").to_string(), window, cx);
        });
        command_history_filter_input.update(cx, |input, cx| {
            input.set_placeholder(t!("filter_command_history").to_string(), window, cx);
        });
        remote_process_filter_input.update(cx, |input, cx| {
            input.set_placeholder(t!("filter_processes").to_string(), window, cx);
        });
        remote_port_filter_input.update(cx, |input, cx| {
            input.set_placeholder(t!("filter_ports").to_string(), window, cx);
        });
        let ui_font_family: SharedString = config.ui_font_family().into();
        let terminal_font_family: SharedString = config.terminal_font_family().into();
        let last_sidebar_width = Some(px(config
            .workspace_panels()
            .and_then(|s| s.first().copied())
            .unwrap_or(constants::SIDEBAR_WIDTH)));
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            selector_focus_handle: cx.focus_handle(),
            host_input,
            session_name_input,
            session_group: String::new(),
            port_input,
            user_input,
            password_input,
            key_path_input,
            key_inline_input,
            passphrase_input,
            baud_rate_input,
            session_protocol: "ssh".to_string(),
            ssh_terminal_encoding: TextEncoding::Utf8,
            ssh_proxy_type: "none".to_string(),
            proxy_host_input,
            proxy_port_input,
            proxy_user_input,
            proxy_password_input,
            global_proxy_type: config.global_proxy_type().to_string(),
            global_proxy_host_input,
            global_proxy_port_input,
            global_proxy_user_input,
            global_proxy_password_input,
            sync_endpoint_input,
            sync_username_input,
            sync_webdav_password_input,
            sync_s3_endpoint_input,
            sync_s3_region_input,
            sync_s3_bucket_input,
            sync_s3_object_key_input,
            sync_s3_access_key_input,
            sync_s3_secret_key_input,
            sync_s3_session_token_input,
            sync_encryption_password_input,
            sync_in_progress: false,
            sync_status: t!("sync_not_run").into(),
            sftp_path_input,
            remote_process_filter_input,
            remote_port_filter_input,
            connection_filter_input,
            connection_group_name_input,
            command_history_filter_input,
            selected_connection_ids: HashSet::new(),
            selected_command_history: HashSet::new(),
            ssh_auth_method: AuthMethod::Password,
            ssh_config_entries: crate::session::ssh_config::parse_ssh_config().unwrap_or_default(),
            ssh_config_selected: None,
            editing_session_id: None,
            editing_connection_group: None,
            follow_system_theme,
            theme_mode,
            light_theme_name,
            dark_theme_name,
            ui_font_size: config.ui_font_size(),
            terminal_font_size: config.terminal_font_size(),
            terminal_zoom_accumulator: 0.0,
            cursor_style: config.cursor_style(),
            ui_font_family,
            terminal_font_family,
            tabs: Vec::new(),
            active_tab: None,
            tab_groups: Vec::new(),
            active_group: None,
            pane_root: PaneLayout::Single(String::new()),
            focused_pane_path: Vec::new(),
            terminal_panel_bounds: None,
            selector_selection: 0,
            workspace_panels,
            body_panels,
            sftp_tree_panels,
            sftp_file_columns,
            is_layout_reset: false,
            terminal_scrollbars: HashMap::new(),
            remote_files_scroll_handle: UniformListScrollHandle::new(),
            remote_files_horizontal_scroll_handle: gpui::ScrollHandle::new(),
            remote_files_columns_viewport_width: None,
            remote_tree_scroll_handle: UniformListScrollHandle::new(),
            disk_scroll_handle: gpui::ScrollHandle::new(),
            process_scroll_handle: gpui::ScrollHandle::new(),
            port_scroll_handle: UniformListScrollHandle::new(),
            tabs_scroll_handle: gpui::ScrollHandle::new(),
            selector_scroll_handle: gpui::ScrollHandle::new(),
            saved_scroll_handle: gpui::ScrollHandle::new(),
            command_history_scroll_handle: gpui::ScrollHandle::new(),
            connection_scroll_handle: gpui::ScrollHandle::new(),
            saved_sessions_overflowing: false,
            connection_progress: None,
            pending_sftp_path_sync: Some("/".into()),
            sftp_context_menu: None,
            sftp_rename_input,
            sftp_rename_state: None,
            sftp_editor_input,
            sftp_editor_state: None,
            sftp_creating_folder: false,
            sftp_new_folder_input,
            sftp_delete_scroll_handle: gpui::ScrollHandle::new(),
            show_hidden_files: config.show_hidden_files(),
            transfers: {
                let mut transfers = config.transfers();
                for t in transfers.iter_mut() {
                    if matches!(
                        t.state,
                        crate::terminal::TransferState::Running
                            | crate::terminal::TransferState::Paused
                    ) {
                        t.state =
                            crate::terminal::TransferState::Zombie(t!("zombie_reason").to_string());
                    }
                }
                transfers
            },
            show_transfers_dialog: false,
            show_command_history: false,
            ssh_command_buffers: HashMap::new(),
            ssh_command_starts: HashMap::new(),
            system_status: None,
            server_monitor_view: ServerMonitorView::default(),
            remote_processes: Vec::new(),
            remote_process_status: None,
            remote_processes_in_flight: false,
            expanded_process_pid: None,
            remote_ports: Vec::new(),
            remote_ports_status: None,
            remote_ports_in_flight: false,
            terminating_processes: HashSet::new(),
            terminal_bounds: HashMap::new(),
            terminal_selecting: false,
            terminal_marked_text: None,
            dragging_splitter: None,
            drag_split_origin: None,
            sftp_panel_minimized: config.sftp_panel_minimized(),
            sidebar_collapsed: config.sidebar_collapsed(),
            window_active: window.is_window_active(),
            native_window_handle: crate::desktop_notification::native_window_handle(window),
            unread_terminal_notifications: HashSet::new(),
            prev_monitoring_size: None,
            status: "ready".into(),
            active_title_bar_style: config.title_bar_style(),
            config,
            system_sampler,
            recording_action: None,
            active_dialog: None,
            keybind_error: None,
            keybinds_suspended: false,
            system,
            cpu_history: Vec::with_capacity(20),
            net_rx_history: Vec::with_capacity(20),
            net_tx_history: Vec::with_capacity(20),
            last_system_sample: Instant::now(),
            last_theme_sync: Instant::now(),

            search_input,
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_current: 0,
            search_target_tab: None,
            search_bar_bounds: None,

            system_tab_id: None,
            sftp_handles: std::collections::HashMap::new(),

            remote_sample_in_flight: false,
            runtime: Runtime::new().expect("create tokio runtime"),
            events_rx,
            events_tx,
            last_window_size: None,
            last_sidebar_width,
            pending_local_terminal_resizes: HashMap::new(),
            local_terminal_resize_task: None,
            hovered_url: None,
            cmd_ctrl_pressed: false,
            _subscriptions,
            last_window_bounds: Some(window.window_bounds()),
            window_bounds_save_task: None,
            save_lock: std::sync::Arc::new(std::sync::Mutex::new(())),
            save_latest_seq: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        };

        this.apply_theme_preferences(window, cx);
        this.restore_saved_tabs(window, cx);
        this.report_active_terminal_focus(this.window_active);
        this.start_event_pump(window, cx);
        this
    }

    pub(crate) fn apply_loaded_config(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.follow_system_theme = self.config.follow_system_theme();
        self.theme_mode = match self.config.theme_mode() {
            "light" => ThemeMode::Light,
            "dark" => ThemeMode::Dark,
            _ => ThemeMode::Dark,
        };
        self.light_theme_name = self.config.light_theme_name().to_string().into();
        self.dark_theme_name = self.config.dark_theme_name().to_string().into();
        self.ui_font_size = self.config.ui_font_size();
        self.terminal_font_size = self.config.terminal_font_size();
        self.cursor_style = self.config.cursor_style();
        self.ui_font_family = self.config.ui_font_family().to_string().into();
        self.terminal_font_family = self.config.terminal_font_family().to_string().into();
        self.show_hidden_files = self.config.show_hidden_files();
        self.sftp_panel_minimized = self.config.sftp_panel_minimized();
        self.sidebar_collapsed = self.config.sidebar_collapsed();
        self.active_title_bar_style = self.config.title_bar_style();

        // Apply theme preferences
        self.apply_theme_preferences(window, cx);

        // Update inputs
        Self::set_input_value(
            &self.sync_endpoint_input,
            self.config.sync_endpoint().to_string(),
            window,
            cx,
        );
        Self::set_input_value(
            &self.sync_username_input,
            self.config.sync_username().to_string(),
            window,
            cx,
        );
        Self::set_input_value(
            &self.sync_s3_endpoint_input,
            self.config.sync_s3_endpoint().to_string(),
            window,
            cx,
        );
        Self::set_input_value(
            &self.sync_s3_region_input,
            self.config.sync_s3_region().to_string(),
            window,
            cx,
        );
        Self::set_input_value(
            &self.sync_s3_bucket_input,
            self.config.sync_s3_bucket().to_string(),
            window,
            cx,
        );
        Self::set_input_value(
            &self.sync_s3_object_key_input,
            self.config.sync_s3_object_key().to_string(),
            window,
            cx,
        );

        // Notify
        cx.notify();
    }

    pub(crate) fn on_input_event(
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
                        if let Some(handle) = self.active_sftp_handle() {
                            let _ = handle
                                .commands
                                .send(crate::sftp::SftpCommand::CreateDir(path));
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
        } else if input == &self.search_input {
            if let InputEvent::PressEnter { .. } = event {
                if self.search_query.is_empty()
                    || *self.search_input.read(cx).text() != self.search_query
                {
                    self.perform_search(window, cx);
                } else {
                    self.search_goto_next(cx);
                }
                window.prevent_default();
                cx.stop_propagation();
            }
        }
        cx.notify();
    }

    pub(crate) fn save_preferences_background(&mut self) {
        let local_config = self.config.cache.clone();
        let config_store = self.config.clone();
        let latest_seq = self.save_latest_seq.clone();
        let current_seq = latest_seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let save_lock = self.save_lock.clone();

        self.runtime.spawn(async move {
            let _ = tokio::task::spawn_blocking(move || {
                let Ok(_guard) = save_lock.lock() else {
                    tracing::error!("failed to lock preferences save state");
                    return;
                };
                if current_seq < latest_seq.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                if let Err(err) = config_store.save_merged_preferences(local_config) {
                    tracing::error!("failed to save merged preferences in background: {err:#}");
                }
            })
            .await;
        });
    }

    fn expire_terminal_activity(&mut self, now: Instant) -> bool {
        self.tabs.iter_mut().fold(false, |changed, tab| {
            tab.expire_output_activity(now) || changed
        })
    }

    fn handle_terminal_notification(&mut self, tab_id: &str, notification: TerminalNotification) {
        let terminal_visible = self.is_terminal_visible(tab_id);
        tracing::info!(
            tab_id,
            terminal_visible,
            source = notification.source.as_str(),
            "received terminal notification"
        );
        if !should_show_terminal_notification(
            notification.occasion,
            self.window_active,
            terminal_visible,
        ) {
            return;
        }

        let fallback_title = self
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .map(|tab| format!("ashell - {}", tab.title))
            .unwrap_or_else(|| "ashell".to_string());
        let title = notification.title.unwrap_or(fallback_title);
        let body = notification.body.unwrap_or_default();
        crate::desktop_notification::show_terminal_notification(title, body);
        if !terminal_visible
            && self
                .unread_terminal_notifications
                .insert(tab_id.to_string())
        {
            self.update_unread_indicator();
        }
    }

    fn is_terminal_visible(&self, tab_id: &str) -> bool {
        if !self.window_active {
            return false;
        }

        self.active_group
            .as_ref()
            .and_then(|group_id| self.tab_groups.iter().find(|group| &group.id == group_id))
            .map(|group| group.pane_root.contains(tab_id))
            .unwrap_or_else(|| self.active_tab.as_deref() == Some(tab_id))
    }

    fn update_unread_indicator(&self) {
        crate::desktop_notification::set_unread_indicator(
            !self.unread_terminal_notifications.is_empty(),
            self.native_window_handle,
        );
    }

    pub(crate) fn clear_visible_terminal_notifications(&mut self) {
        if !self.window_active {
            return;
        }

        let visible_tab_ids = self
            .active_group
            .as_ref()
            .and_then(|group_id| self.tab_groups.iter().find(|group| &group.id == group_id))
            .map(|group| {
                group
                    .pane_root
                    .tab_ids()
                    .into_iter()
                    .map(ToOwned::to_owned)
                    .collect::<HashSet<_>>()
            })
            .or_else(|| {
                self.active_tab
                    .as_ref()
                    .map(|tab_id| HashSet::from([tab_id.clone()]))
            })
            .unwrap_or_default();
        let previous_count = self.unread_terminal_notifications.len();
        self.unread_terminal_notifications
            .retain(|tab_id| !visible_tab_ids.contains(tab_id));
        if previous_count != self.unread_terminal_notifications.len() {
            self.update_unread_indicator();
        }
    }

    pub(crate) fn clear_closed_terminal_notifications(&mut self) {
        let open_tab_ids = self
            .tabs
            .iter()
            .map(|tab| tab.id.clone())
            .collect::<HashSet<_>>();
        let previous_count = self.unread_terminal_notifications.len();
        self.unread_terminal_notifications
            .retain(|tab_id| open_tab_ids.contains(tab_id));
        if previous_count != self.unread_terminal_notifications.len() {
            self.update_unread_indicator();
        }
    }

    pub(crate) fn update_terminal_focus(&mut self, previous_tab_id: Option<&str>) {
        let active_tab_id = self.active_tab.clone();
        if previous_tab_id != active_tab_id.as_deref() {
            if let Some(previous_tab_id) = previous_tab_id {
                if let Some(tab) = self.tabs.iter().find(|tab| tab.id == previous_tab_id) {
                    tab.report_focus(false);
                }
            }
            self.report_active_terminal_focus(self.window_active);
        }
        self.clear_visible_terminal_notifications();
    }

    fn report_active_terminal_focus(&self, focused: bool) {
        if let Some(tab) = self
            .active_tab
            .as_ref()
            .and_then(|tab_id| self.tabs.iter().find(|tab| &tab.id == tab_id))
        {
            tab.report_focus(focused);
        }
    }

    fn on_window_activation_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_window_activation(window);
        cx.notify();
    }

    fn sync_window_activation(&mut self, window: &Window) -> bool {
        let window_active = window.is_window_active();
        if self.window_active == window_active {
            return false;
        }

        self.window_active = window_active;
        self.report_active_terminal_focus(window_active);
        self.clear_visible_terminal_notifications();
        true
    }

    pub(crate) fn start_event_pump(&self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let mut last_blink_time = std::time::Instant::now();
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                if this
                    .update_in(cx, |this, window, cx| {
                        let activation_changed = this.sync_window_activation(window);
                        let changed = this.drain_backend_events(cx);
                        let system_sampled = this.sample_system_if_due();
                        this.sync_theme_if_due(cx);
                        let is_blinking = matches!(
                            this.cursor_style,
                            crate::session::config::CursorStyle::Blink
                                | crate::session::config::CursorStyle::BeamBlink
                        );
                        let now = std::time::Instant::now();
                        let activity_changed = this.expire_terminal_activity(now);
                        let blink_due = is_blinking
                            && now.duration_since(last_blink_time)
                                >= std::time::Duration::from_millis(600);
                        if activation_changed
                            || changed
                            || system_sampled
                            || activity_changed
                            || blink_due
                        {
                            cx.notify();
                            if blink_due {
                                last_blink_time = now;
                            }
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    pub(crate) fn drain_backend_events(&mut self, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
        let mut transfers_changed = false;
        while let Ok(event) = self.events_rx.try_recv() {
            let Some(event) = event.into_current() else {
                continue;
            };
            changed = true;
            match event {
                BackendEvent::Guarded { .. } => unreachable!("guarded events are unwrapped above"),
                BackendEvent::Output { tab_id, bytes } => {
                    let notifications = self
                        .tabs
                        .iter_mut()
                        .find(|tab| tab.id == tab_id)
                        .map(|tab| tab.feed(&bytes))
                        .unwrap_or_default();
                    for notification in notifications {
                        self.handle_terminal_notification(&tab_id, notification);
                    }
                }
                BackendEvent::Status { tab_id, text } => {
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                        tab.status = text.clone();
                    }
                    if let Some(progress) = self.connection_progress.as_mut() {
                        if progress.tab_id == tab_id {
                            progress.lines.push(text.clone().into());
                            let _idx = progress.lines.len().saturating_sub(1);
                            self.connection_scroll_handle
                                .set_offset(gpui::point(px(0.), px(-99999.0)));
                        }
                    }
                    self.status = text.into();
                }
                BackendEvent::Connected { tab_id } => {
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                        tab.connected = true;
                        tab.disconnected_reason = None;
                    }
                    self.sync_sftp_path_from_terminal_title(&tab_id, cx);
                    self.sync_system_tab_to_active_group();
                    if self.system_tab_id.as_deref() == Some(tab_id.as_str()) {
                        self.system_status = None;
                        self.remote_process_status = None;
                        self.remote_ports_status = None;
                    }
                    self.request_active_system_snapshot();
                    if self.active_dialog == Some(DialogKind::Processes) {
                        self.request_active_process_snapshot();
                    }
                    if self.active_dialog == Some(DialogKind::Ports) {
                        self.request_active_port_snapshot();
                    }
                    if self
                        .connection_progress
                        .as_ref()
                        .is_some_and(|progress| progress.tab_id == tab_id && !progress.failed)
                    {
                        self.connection_progress = None;
                    }
                }
                BackendEvent::SftpEntries {
                    tab_id,
                    path,
                    entries,
                } => {
                    let mut active_path = None;
                    if let Some(group) = self
                        .tab_groups
                        .iter_mut()
                        .find(|group| group.sftp_tab_id.as_deref() == Some(tab_id.as_str()))
                    {
                        if let Some(sftp) = group.sftp.as_mut() {
                            let normalized_path =
                                crate::sftp::normalize_remote_path(&path, "/", &sftp.home_dir);
                            let is_current = sftp.current_path == normalized_path;
                            sftp.apply_directory_entries(normalized_path, entries);
                            if is_current && self.active_group.as_deref() == Some(group.id.as_str())
                            {
                                active_path = Some(sftp.current_path.clone());
                            }
                        }
                    }
                    if let Some(path) = active_path {
                        self.pending_sftp_path_sync = Some(path);
                    }
                }
                BackendEvent::SftpDirectoryFailed {
                    tab_id,
                    path,
                    reason,
                } => {
                    if let Some(group) = self
                        .tab_groups
                        .iter_mut()
                        .find(|group| group.sftp_tab_id.as_deref() == Some(tab_id.as_str()))
                    {
                        if let Some(sftp) = group.sftp.as_mut() {
                            let normalized_path =
                                crate::sftp::normalize_remote_path(&path, "/", &sftp.home_dir);
                            sftp.apply_directory_error(normalized_path, reason);
                        }
                    }
                }
                BackendEvent::SftpPreview { tab_id, preview } => {
                    if let Some(group) = self
                        .tab_groups
                        .iter_mut()
                        .find(|group| group.sftp_tab_id.as_deref() == Some(tab_id.as_str()))
                    {
                        if let Some(sftp) = group.sftp.as_mut() {
                            sftp.selected_path = Some(preview.path.clone());
                            sftp.preview = Some(preview);
                        }
                    }
                }
                BackendEvent::SftpStatus { tab_id, text } => {
                    let group_id = self
                        .tab_groups
                        .iter_mut()
                        .find(|group| group.sftp_tab_id.as_deref() == Some(tab_id.as_str()))
                        .map(|group| {
                            if let Some(sftp) = group.sftp.as_mut() {
                                sftp.status = text.clone();
                            }
                            group.id.clone()
                        });
                    if group_id
                        .as_ref()
                        .is_some_and(|group_id| self.active_group.as_ref() == Some(group_id))
                    {
                        self.status = text.into();
                    }
                }
                BackendEvent::RemoteSystem { tab_id, snapshot } => {
                    if self.is_connected_system_tab(&tab_id) {
                        self.remote_sample_in_flight = false;
                        self.system_status = None;
                        self.system = snapshot.clone();
                        self.cpu_history.push(snapshot.cpu_percent);
                        if self.cpu_history.len() > 20 {
                            self.cpu_history.remove(0);
                        }
                        self.net_rx_history.push(snapshot.net_rx_rate as f32);
                        if self.net_rx_history.len() > 20 {
                            self.net_rx_history.remove(0);
                        }
                        self.net_tx_history.push(snapshot.net_tx_rate as f32);
                        if self.net_tx_history.len() > 20 {
                            self.net_tx_history.remove(0);
                        }
                    }
                }
                BackendEvent::RemoteSystemUnavailable { tab_id, reason } => {
                    if self.is_connected_system_tab(&tab_id) {
                        self.remote_sample_in_flight = false;
                        self.system_status = Some(reason.clone().into());
                        self.status = reason.into();
                    }
                }
                BackendEvent::RemoteProcesses { tab_id, processes } => {
                    if self.is_connected_system_tab(&tab_id) {
                        self.remote_processes_in_flight = false;
                        self.remote_process_status = None;
                        self.remote_processes = processes;
                        self.sort_remote_processes();
                    }
                }
                BackendEvent::RemoteProcessesUnavailable { tab_id, reason } => {
                    if self.is_connected_system_tab(&tab_id) {
                        self.remote_processes_in_flight = false;
                        self.remote_processes.clear();
                        self.remote_process_status = Some(reason.clone().into());
                        self.status = reason.into();
                    }
                }
                BackendEvent::RemotePorts { tab_id, ports } => {
                    if self.is_connected_system_tab(&tab_id) {
                        self.remote_ports_in_flight = false;
                        self.remote_ports_status = None;
                        self.remote_ports = ports;
                        self.sort_remote_ports();
                    }
                }
                BackendEvent::RemotePortsUnavailable { tab_id, reason } => {
                    if self.is_connected_system_tab(&tab_id) {
                        self.remote_ports_in_flight = false;
                        self.remote_ports.clear();
                        self.remote_ports_status = Some(reason.clone().into());
                        self.status = reason.into();
                    }
                }
                BackendEvent::RemoteProcessTerminated { tab_id, pid } => {
                    if self.is_connected_system_tab(&tab_id) {
                        self.terminating_processes.remove(&pid);
                        self.remote_processes.retain(|process| process.pid != pid);
                        if self.expanded_process_pid == Some(pid) {
                            self.expanded_process_pid = None;
                        }
                        self.remote_process_status =
                            Some(t!("process_terminated", pid = pid).to_string().into());
                        self.status = t!("process_terminated", pid = pid).to_string().into();
                        self.request_active_process_snapshot();
                    }
                }
                BackendEvent::RemoteProcessTerminateFailed {
                    tab_id,
                    pid,
                    reason,
                } => {
                    if self.is_connected_system_tab(&tab_id) {
                        self.terminating_processes.remove(&pid);
                        let message =
                            t!("process_terminate_failed", pid = pid, reason = reason).to_string();
                        self.remote_process_status = Some(message.clone().into());
                        self.status = message.into();
                    }
                }
                BackendEvent::Closed { tab_id, reason } => {
                    let is_graceful_exit =
                        reason == "local shell closed" || reason == "ssh session closed";
                    if is_graceful_exit {
                        self.handle_tab_close(tab_id.clone());
                        self.status = reason.into();
                        continue;
                    }
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                        tab.clear_command_activity();
                        tab.connected = false;
                        tab.status = reason.clone();
                        tab.disconnected_reason = Some(reason.clone());
                    }
                    if self.system_tab_id.as_deref() == Some(tab_id.as_str()) {
                        self.system = SystemSnapshot::default();
                        self.cpu_history.clear();
                        self.net_rx_history.clear();
                        self.net_tx_history.clear();
                        self.remote_sample_in_flight = false;
                        self.remote_processes_in_flight = false;
                        self.remote_processes.clear();
                        self.remote_ports_in_flight = false;
                        self.remote_ports.clear();
                        self.terminating_processes.clear();
                        self.system_status = Some(reason.clone().into());
                        self.remote_process_status = Some(reason.clone().into());
                        self.remote_ports_status = Some(reason.clone().into());
                    }
                    if let Some(progress) = self.connection_progress.as_mut() {
                        if progress.tab_id == tab_id {
                            progress.lines.push(reason.clone().into());
                            let _idx = progress.lines.len().saturating_sub(1);
                            self.connection_scroll_handle
                                .set_offset(gpui::point(px(0.), px(-99999.0)));
                            progress.title = t!("connection_failed").into();
                            progress.failed = true;
                        }
                    }
                    self.status = reason.into();
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
                BackendEvent::TransferStarted { tab_id, info } => {
                    let tab_title = self.transfer_source_title(&tab_id);
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
                    if self.transfers.len() > 100 {
                        self.transfers.truncate(100);
                    }
                    transfers_changed = true;
                }
                BackendEvent::SftpHome { tab_id, home } => {
                    let home = crate::sftp::normalize_remote_path(&home, "/", "/");
                    let mut group_id = None;
                    let mut paths_to_load = Vec::new();
                    let mut sync_active_path = false;
                    if let Some(group) = self
                        .tab_groups
                        .iter_mut()
                        .find(|group| group.sftp_tab_id.as_deref() == Some(tab_id.as_str()))
                    {
                        if let Some(sftp) = group.sftp.as_mut() {
                            sftp.home_dir = home.clone();
                            sftp.home_dir_resolved = true;
                            sftp.current_path = home.clone();
                            sftp.expand_to(&home);
                            sftp.begin_directory_load(&home);
                            for path in crate::sftp::remote_path_ancestors(&home) {
                                if path != home
                                    && !sftp.directory_cache.contains_key(&path)
                                    && !sftp.loading_directories.contains(&path)
                                {
                                    sftp.begin_directory_load(&path);
                                    paths_to_load.push(path);
                                }
                            }
                            group_id = Some(group.id.clone());
                            sync_active_path =
                                self.active_group.as_deref() == Some(group.id.as_str());
                        }
                    }
                    if let Some(handle) = group_id.as_ref().and_then(|id| self.sftp_handles.get(id))
                    {
                        for path in paths_to_load {
                            handle.list_dir(path);
                        }
                    }
                    if sync_active_path {
                        self.pending_sftp_path_sync = Some(home);
                        self.sync_sftp_path_from_terminal_title(&tab_id, cx);
                    }
                }
                BackendEvent::TerminalTitleChanged { tab_id, title } => {
                    let local_path = title
                        .strip_prefix("ASHELL_CWD_B64:")
                        .and_then(crate::session::decode_local_path_title);
                    if let Some(path) = local_path {
                        self.apply_local_directory_change(&tab_id, path);
                    } else if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                        tab.dynamic_title = title;
                        tab.terminal_title_received = true;
                    }
                    self.sync_sftp_path_from_terminal_title(&tab_id, cx);
                }
                BackendEvent::TerminalBell { tab_id } => {
                    let body = t!("terminal_attention_required").to_string();
                    self.handle_terminal_notification(&tab_id, TerminalNotification::bell(body));
                }
                BackendEvent::LocalDirectoryChanged { tab_id, path } => {
                    self.apply_local_directory_change(&tab_id, path);
                }
                BackendEvent::SyncFinished(result) => {
                    self.sync_in_progress = false;
                    match result {
                        crate::sync::SyncResult::Uploaded { etag } => {
                            if etag.is_some() {
                                self.config.set_sync_etag(etag);
                            }
                            self.sync_status = t!("sync_upload_complete").into();
                            let _ = self.config.save();
                        }
                        crate::sync::SyncResult::Downloaded { payload, etag } => {
                            self.config.replace_sessions(payload.sessions);
                            self.selected_connection_ids.clear();
                            self.config.set_sync_etag(etag);
                            match self.config.save() {
                                Ok(()) => self.sync_status = t!("sync_download_complete").into(),
                                Err(err) => {
                                    self.sync_status =
                                        format!("{}: {err:#}", t!("sync_failed")).into()
                                }
                            }
                        }
                        crate::sync::SyncResult::Failed(error) => {
                            self.sync_status = format!("{}: {error}", t!("sync_failed")).into();
                        }
                    }
                }
            }
        }
        if transfers_changed {
            self.config.set_transfers(self.transfers.clone());
        }
        changed
    }

    pub(crate) fn sample_system_if_due(&mut self) -> bool {
        if self.last_system_sample.elapsed() >= SystemSampler::interval() {
            self.last_system_sample = Instant::now();
            if let Some(ref tab_id) = self.system_tab_id.clone() {
                let ssh_connected = self
                    .tabs
                    .iter()
                    .find(|tab| tab.id == *tab_id && tab.kind == TabKind::Ssh)
                    .map(|tab| tab.connected);
                if let Some(connected) = ssh_connected {
                    if connected {
                        self.request_active_system_snapshot();
                        if self.active_dialog == Some(DialogKind::Processes) {
                            self.request_active_process_snapshot();
                        }
                        if self.active_dialog == Some(DialogKind::Ports) {
                            self.request_active_port_snapshot();
                        }
                    }
                    return false;
                }
            }
            let snapshot = self.system_sampler.sample();
            let cpu_usage = snapshot.cpu_percent;
            self.cpu_history.push(cpu_usage);
            if self.cpu_history.len() > 20 {
                self.cpu_history.remove(0);
            }
            self.net_rx_history.push(snapshot.net_rx_rate as f32);
            if self.net_rx_history.len() > 20 {
                self.net_rx_history.remove(0);
            }
            self.net_tx_history.push(snapshot.net_tx_rate as f32);
            if self.net_tx_history.len() > 20 {
                self.net_tx_history.remove(0);
            }
            self.system = snapshot;
            return true;
        }
        false
    }

    pub(crate) fn sync_theme_if_due(&mut self, cx: &mut Context<Self>) {
        if self.follow_system_theme && self.last_theme_sync.elapsed() >= Duration::from_secs(1) {
            self.last_theme_sync = Instant::now();
            Theme::sync_system_appearance(None, cx);
            cx.refresh_windows();
        }
    }

    pub(crate) fn request_active_system_snapshot(&mut self) {
        let Some(ref tab_id) = self.system_tab_id.clone() else {
            return;
        };
        let Some(backend) = (|| {
            let tab = self.tabs.iter().find(|t| t.id == *tab_id)?;
            if !tab.connected {
                return None;
            }
            Some(tab.backend.clone())
        })() else {
            return;
        };
        if self.remote_sample_in_flight {
            return;
        }
        if let Ok(backend) = backend.lock() {
            self.remote_sample_in_flight = true;
            backend.send(crate::terminal::BackendCommand::SampleMetrics);
        }
    }

    fn is_connected_system_tab(&self, tab_id: &str) -> bool {
        self.system_tab_id.as_deref() == Some(tab_id)
            && self
                .tabs
                .iter()
                .any(|tab| tab.id == tab_id && tab.kind == TabKind::Ssh && tab.connected)
    }

    pub(crate) fn request_active_process_snapshot(&mut self) {
        let Some(ref tab_id) = self.system_tab_id.clone() else {
            return;
        };
        let Some(backend) = (|| {
            let tab = self
                .tabs
                .iter()
                .find(|tab| tab.id == *tab_id && tab.kind == TabKind::Ssh && tab.connected)?;
            Some(tab.backend.clone())
        })() else {
            return;
        };
        if self.remote_processes_in_flight {
            return;
        }
        if let Ok(backend) = backend.lock() {
            self.remote_processes_in_flight = true;
            if self.remote_processes.is_empty() {
                self.remote_process_status = Some(t!("loading_processes").to_string().into());
            }
            backend.send(crate::terminal::BackendCommand::SampleProcesses);
        }
    }

    pub(crate) fn request_active_port_snapshot(&mut self) {
        let Some(ref tab_id) = self.system_tab_id.clone() else {
            return;
        };
        let Some(backend) = (|| {
            let tab = self
                .tabs
                .iter()
                .find(|tab| tab.id == *tab_id && tab.kind == TabKind::Ssh && tab.connected)?;
            Some(tab.backend.clone())
        })() else {
            return;
        };
        if self.remote_ports_in_flight {
            return;
        }
        if let Ok(backend) = backend.lock() {
            self.remote_ports_in_flight = true;
            if self.remote_ports.is_empty() {
                self.remote_ports_status = Some(t!("loading_ports").to_string().into());
            }
            backend.send(crate::terminal::BackendCommand::SamplePorts);
        }
    }

    fn sort_remote_ports(&mut self) {
        self.remote_ports.sort_by(|a, b| {
            a.port
                .cmp(&b.port)
                .then_with(|| a.protocol.cmp(&b.protocol))
                .then_with(|| a.address.cmp(&b.address))
                .then_with(|| a.pid.cmp(&b.pid))
        });
    }

    pub(crate) fn toggle_process_expanded(&mut self, pid: u32, cx: &mut Context<Self>) {
        self.expanded_process_pid = (self.expanded_process_pid != Some(pid)).then_some(pid);
        cx.notify();
    }

    fn sort_remote_processes(&mut self) {
        match self.server_monitor_view {
            ServerMonitorView::Cpu => self.remote_processes.sort_by(|a, b| {
                b.cpu_percent
                    .total_cmp(&a.cpu_percent)
                    .then_with(|| a.pid.cmp(&b.pid))
            }),
            ServerMonitorView::Memory => self.remote_processes.sort_by(|a, b| {
                b.memory_bytes
                    .cmp(&a.memory_bytes)
                    .then_with(|| a.pid.cmp(&b.pid))
            }),
        }
    }

    pub(crate) fn terminate_remote_process(
        &mut self,
        tab_id: String,
        pid: u32,
        cx: &mut Context<Self>,
    ) {
        if pid <= 1 || self.terminating_processes.contains(&pid) {
            return;
        }
        if self.system_tab_id.as_deref() != Some(tab_id.as_str()) {
            return;
        }
        let Some(backend) = (|| {
            let tab = self
                .tabs
                .iter()
                .find(|tab| tab.id == tab_id && tab.kind == TabKind::Ssh && tab.connected)?;
            Some(tab.backend.clone())
        })() else {
            return;
        };
        if let Ok(backend) = backend.lock() {
            self.terminating_processes.insert(pid);
            self.remote_process_status =
                Some(t!("terminating_process", pid = pid).to_string().into());
            backend.send(crate::terminal::BackendCommand::TerminateProcess { pid });
        }
        cx.notify();
    }

    pub(crate) fn terminal_ime_bounds_for_range(
        &self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        cell_width: f32,
        line_height: f32,
    ) -> Option<Bounds<Pixels>> {
        let snapshot = self.active_snapshot()?;
        let cursor = snapshot.cursor?;
        let x = element_bounds.origin.x
            + px(cell_width) * cursor.col as f32
            + px(cell_width) * range_utf16.start as f32;
        let y = element_bounds.origin.y + px(line_height) * cursor.row as f32;
        Some(Bounds::new(
            point(x, y),
            size(px(cell_width), px(line_height)),
        ))
    }

    pub(crate) fn remove_transfer(&mut self, transfer_id: &str, cx: &mut Context<Self>) {
        self.transfers.retain(|t| t.info.id != transfer_id);
        self.config.set_transfers(self.transfers.clone());
        cx.notify();
    }

    pub(crate) fn retry_connection_progress(&mut self, cx: &mut Context<Self>) {
        let Some(progress) = self.connection_progress.clone() else {
            return;
        };
        self.connection_progress = None;
        let mut retry_tabs = Vec::new();
        for (ix, tab) in self.tabs.iter().enumerate() {
            if !tab.connected && tab.session.is_some() && tab.id == progress.tab_id {
                retry_tabs.push((ix, tab.id.clone(), tab.session.clone().unwrap(), tab.kind));
            }
        }

        if retry_tabs.is_empty() {
            cx.notify();
            return;
        }

        for (ix, tab_id, session, tab_kind) in retry_tabs {
            let backend_events = self.tabs[ix].advance_backend_events();
            // Invalidate old backend events before requesting shutdown.
            self.tabs[ix].send_backend(crate::terminal::BackendCommand::Close);

            // Spawn new backend
            let backend = match tab_kind {
                crate::terminal::TabKind::Serial => {
                    let b = crate::backend::serial::spawn_serial_client(
                        self.runtime.handle(),
                        tab_id.clone(),
                        session.clone(),
                        backend_events.clone(),
                    );
                    crate::terminal::BackendTx::Serial(b)
                }
                crate::terminal::TabKind::Ssh => crate::backend::ssh::spawn_ssh_terminal(
                    self.runtime.handle(),
                    tab_id.clone(),
                    session.clone(),
                    self.tabs[ix].cols,
                    self.tabs[ix].rows,
                    backend_events.clone(),
                ),
                _ => continue,
            };

            // Replace tab state
            self.tabs[ix].set_backend(backend);
            self.tabs[ix].connected = false;
            self.tabs[ix].status = "connecting".into();
            self.tabs[ix].disconnected_reason = None;
            self.tabs[ix].terminal_title_received = false;

            if tab_kind == crate::terminal::TabKind::Ssh
                && self.active_tab.as_deref() == Some(tab_id.as_str())
            {
                self.restart_active_sftp();
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
        if let Some(progress) = &self.connection_progress {
            let tab_id = progress.tab_id.clone();
            self.connection_progress = None;
            self.handle_tab_close(tab_id);
        }
        cx.notify();
    }

    pub(crate) fn sync_cwd_from_terminal(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let active_id = self.active_tab.clone();
        let Some(active_id) = active_id else {
            return;
        };

        let parsed = if let Some(tab) = self.tabs.iter().find(|t| t.id == active_id) {
            let home_dir = if let Some(group) = self
                .tab_groups
                .iter()
                .find(|g| g.pane_root.contains(&tab.id))
            {
                group
                    .sftp
                    .as_ref()
                    .map(|s| s.home_dir.as_str())
                    .unwrap_or("/")
            } else {
                "/"
            };

            Self::parse_path_from_title(&tab.dynamic_title, home_dir)
        } else {
            None
        };

        if let Some(path) = parsed {
            self.navigate_sftp(path, cx);
        }
    }

    fn sync_sftp_path_from_terminal_title(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        if self.active_tab.as_deref() != Some(tab_id) {
            return;
        }

        let Some(tab) = self.tabs.iter().find(|tab| tab.id == tab_id) else {
            return;
        };
        if tab.kind != TabKind::Ssh
            || !tab.connected
            || !tab.terminal_title_received
            || tab.is_alternate_screen_active()
        {
            return;
        }
        let title = tab.dynamic_title.clone();

        let Some((current_path, home_dir)) = self
            .active_group
            .as_ref()
            .and_then(|group_id| {
                self.tab_groups
                    .iter()
                    .find(|group| &group.id == group_id && group.pane_root.contains(tab_id))
            })
            .and_then(|group| group.sftp.as_ref())
            .filter(|sftp| sftp.home_dir_resolved)
            .map(|sftp| (sftp.current_path.clone(), sftp.home_dir.clone()))
        else {
            return;
        };

        let Some(path) = Self::parse_path_from_title(&title, &home_dir) else {
            return;
        };
        let path = crate::sftp::normalize_remote_path(&path, &current_path, &home_dir);
        if path != current_path {
            self.navigate_sftp(path, cx);
        }
    }

    fn parse_path_from_title(title: &str, home_dir: &str) -> Option<String> {
        let title = title.strip_prefix("ASHELL_CWD:").unwrap_or(title);
        let path_part = if let Some(pos) = title.find(':') {
            title[pos + 1..].trim()
        } else {
            title.trim()
        };

        if path_part.starts_with('/') {
            Some(path_part.to_string())
        } else if path_part == "~" {
            Some(home_dir.to_string())
        } else if let Some(rest) = path_part.strip_prefix("~/") {
            let home = home_dir.trim_end_matches('/');
            Some(format!("{}/{}", home, rest))
        } else {
            None
        }
    }

    fn capture_layout_state(&mut self, window: &mut gpui::Window, cx: &gpui::App) -> bool {
        if self.is_layout_reset {
            tracing::info!("[ui] layout was reset, skipping save layout state.");
            return false;
        }
        let current_bounds = window.window_bounds();
        let bounds = match current_bounds {
            gpui::WindowBounds::Fullscreen(b) => b,
            gpui::WindowBounds::Maximized(b) => b,
            gpui::WindowBounds::Windowed(b) => b,
        };
        let size = bounds.size;
        if size.width.as_f32() > 400.0 && size.height.as_f32() > 300.0 {
            let saved_bounds = match current_bounds {
                gpui::WindowBounds::Fullscreen(b) => {
                    crate::session::config::SavedWindowBounds::Fullscreen {
                        x: b.origin.x.into(),
                        y: b.origin.y.into(),
                        width: b.size.width.into(),
                        height: b.size.height.into(),
                    }
                }
                gpui::WindowBounds::Maximized(b) => {
                    crate::session::config::SavedWindowBounds::Maximized {
                        x: b.origin.x.into(),
                        y: b.origin.y.into(),
                        width: b.size.width.into(),
                        height: b.size.height.into(),
                    }
                }
                gpui::WindowBounds::Windowed(b) => {
                    crate::session::config::SavedWindowBounds::Windowed {
                        x: b.origin.x.into(),
                        y: b.origin.y.into(),
                        width: b.size.width.into(),
                        height: b.size.height.into(),
                    }
                }
            };
            let workspace_sizes: Vec<f32> = self
                .workspace_panels
                .read(cx)
                .sizes()
                .iter()
                .map(|s| s.into())
                .collect();
            let mut body_sizes: Vec<f32> = self
                .body_panels
                .read(cx)
                .sizes()
                .iter()
                .map(|s| s.into())
                .collect();

            if body_sizes.len() < 2 {
                body_sizes = self.config.body_panels().cloned().unwrap_or_default();
            }

            let mut sftp_tree_sizes: Vec<f32> = self
                .sftp_tree_panels
                .read(cx)
                .sizes()
                .iter()
                .map(|size| size.as_f32())
                .collect();
            if sftp_tree_sizes.len() < 2 {
                sftp_tree_sizes = self.config.sftp_tree_panels().cloned().unwrap_or_default();
            }

            let current_sftp_file_column_sizes: Vec<f32> = self
                .sftp_file_columns
                .read(cx)
                .sizes()
                .iter()
                .map(|size| size.as_f32())
                .collect();
            let mut sftp_file_column_sizes = self
                .config
                .sftp_file_columns()
                .filter(|sizes| sizes.len() >= 3)
                .cloned()
                .unwrap_or_else(|| vec![200., 64., 128.]);
            for (index, size) in current_sftp_file_column_sizes
                .into_iter()
                .take(3)
                .enumerate()
            {
                sftp_file_column_sizes[index] = size;
            }

            if self.sftp_panel_minimized {
                if let Some(prev) = self.prev_monitoring_size {
                    if body_sizes.len() > 1 {
                        body_sizes[1] = prev.into();
                    }
                }
            }

            let body_sizes = (!body_sizes.is_empty()).then_some(body_sizes);
            let sftp_tree_sizes = (!sftp_tree_sizes.is_empty()).then_some(sftp_tree_sizes);
            let sftp_file_column_sizes =
                (!sftp_file_column_sizes.is_empty()).then_some(sftp_file_column_sizes);
            self.config
                .set_layout_state(Some(saved_bounds), Some(workspace_sizes), body_sizes);
            self.config.set_sftp_tree_panels(sftp_tree_sizes);
            self.config.set_sftp_file_columns(sftp_file_column_sizes);
            self.config.set_sidebar_collapsed(self.sidebar_collapsed);
            self.config
                .set_sftp_panel_minimized(self.sftp_panel_minimized);
            true
        } else {
            tracing::warn!(
                "[ui] window size is too small ({:?}), skipping save layout state to prevent corrupting saved bounds.",
                size
            );
            false
        }
    }

    pub(crate) fn save_layout_state(&mut self, window: &mut gpui::Window, cx: &gpui::App) {
        let should_save_tabs = self.config.remember_tabs();
        if should_save_tabs {
            self.capture_tabs_state();
        }
        if self.capture_layout_state(window, cx) || should_save_tabs {
            let current_seq = self
                .save_latest_seq
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            let Ok(_guard) = self.save_lock.lock() else {
                tracing::error!("failed to lock window layout save state");
                return;
            };
            if current_seq
                < self
                    .save_latest_seq
                    .load(std::sync::atomic::Ordering::SeqCst)
            {
                return;
            }
            if let Err(err) = self.config.save() {
                tracing::error!("failed to save window layout state: {err:#}");
            }
        }
    }

    fn on_window_bounds_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current_bounds = window.window_bounds();

        // Keep a reset effective until the first actual bounds change. GPUI may
        // notify observers during a redraw without changing the window geometry.
        if self.is_layout_reset && self.last_window_bounds != Some(current_bounds) {
            self.is_layout_reset = false;
        }

        if self.last_window_bounds == Some(current_bounds) {
            return;
        }
        self.last_window_bounds = Some(current_bounds);

        self.window_bounds_save_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.capture_layout_state(window, cx) {
                    this.save_preferences_background();
                }
            });
        }));
    }

    fn save_layout_on_app_quit(&mut self, cx: &mut Context<Self>) -> gpui::Task<()> {
        crate::desktop_notification::clear_unread_indicator(self.native_window_handle);
        let entity_id = cx.entity_id();
        let _ = cx.with_window(entity_id, |window, cx| {
            self.save_layout_state(window, cx);
        });
        gpui::Task::ready(())
    }
}

#[cfg(test)]
mod terminal_notification_tests {
    use super::{TerminalNotificationOccasion, should_show_terminal_notification};

    #[test]
    fn applies_terminal_notification_occasion_rules() {
        assert!(should_show_terminal_notification(
            TerminalNotificationOccasion::Always,
            true,
            true,
        ));
        assert!(!should_show_terminal_notification(
            TerminalNotificationOccasion::Unfocused,
            true,
            false,
        ));
        assert!(should_show_terminal_notification(
            TerminalNotificationOccasion::Unfocused,
            false,
            false,
        ));
        assert!(!should_show_terminal_notification(
            TerminalNotificationOccasion::Invisible,
            true,
            true,
        ));
        assert!(should_show_terminal_notification(
            TerminalNotificationOccasion::Invisible,
            true,
            false,
        ));
    }
}

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::{Context, Result};
use argon2::Argon2;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit},
};
use directories::BaseDirs;
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::text_encoding::TextEncoding;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuthMethod {
    Password,
    Key,
    Config,
}

fn default_protocol() -> String {
    "ssh".to_string()
}

fn default_baud_rate() -> u32 {
    115200
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthMethod,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub private_key_path: String,
    #[serde(default)]
    pub private_key_inline: String,
    #[serde(default)]
    pub passphrase: String,
    #[serde(default)]
    pub last_used: Option<String>,
    #[serde(default = "default_global_proxy_type")]
    pub proxy_type: String, // "none", "socks5", "http"
    #[serde(default)]
    pub proxy_host: String,
    #[serde(default)]
    pub proxy_port: Option<u16>,
    #[serde(default)]
    pub proxy_user: String,
    #[serde(default)]
    pub proxy_password: String,
    #[serde(default = "default_protocol")]
    pub protocol: String,
    #[serde(default = "default_baud_rate")]
    pub baud_rate: u32,
    #[serde(default)]
    pub terminal_encoding: TextEncoding,
}

impl Session {
    pub fn password(host: String, port: u16, user: String, password: String) -> Self {
        let name = format!("{user}@{host}");
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            host,
            port,
            user,
            auth: AuthMethod::Password,
            password,
            private_key_path: String::new(),
            private_key_inline: String::new(),
            passphrase: String::new(),
            last_used: None,
            proxy_type: "none".to_string(),
            proxy_host: String::new(),
            proxy_port: None,
            proxy_user: String::new(),
            proxy_password: String::new(),
            protocol: "ssh".to_string(),
            baud_rate: 115200,
            terminal_encoding: TextEncoding::Utf8,
        }
    }

    pub fn key(
        host: String,
        port: u16,
        user: String,
        private_key_path: String,
        private_key_inline: String,
        passphrase: String,
    ) -> Self {
        let name = format!("{user}@{host}");
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            host,
            port,
            user,
            auth: AuthMethod::Key,
            password: String::new(),
            private_key_path,
            private_key_inline,
            passphrase,
            last_used: None,
            proxy_type: "none".to_string(),
            proxy_host: String::new(),
            proxy_port: None,
            proxy_user: String::new(),
            proxy_password: String::new(),
            protocol: "ssh".to_string(),
            baud_rate: 115200,
            terminal_encoding: TextEncoding::Utf8,
        }
    }

    pub fn serial(port_name: String, baud_rate: u32) -> Self {
        let name = format!("serial://{port_name}@{baud_rate}");
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            host: port_name,
            port: 0,
            user: String::new(),
            auth: AuthMethod::Password,
            password: String::new(),
            private_key_path: String::new(),
            private_key_inline: String::new(),
            passphrase: String::new(),
            last_used: None,
            proxy_type: "none".to_string(),
            proxy_host: String::new(),
            proxy_port: None,
            proxy_user: String::new(),
            proxy_password: String::new(),
            protocol: "serial".to_string(),
            baud_rate,
            terminal_encoding: TextEncoding::Utf8,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SavedWindowBounds {
    Fullscreen {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
    Maximized {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
    Windowed {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SavedPaneLayout {
    Single {
        tab_id: String,
    },
    Horizontal {
        children: Vec<SavedPaneLayout>,
        ratio: f32,
    },
    Vertical {
        children: Vec<SavedPaneLayout>,
        ratio: f32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SavedTerminalTab {
    Local {
        id: String,
        #[serde(default)]
        cwd: Option<PathBuf>,
        #[serde(default)]
        terminal_encoding: TextEncoding,
    },
    Ssh {
        id: String,
        session: Session,
    },
    Serial {
        id: String,
        session: Session,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedTabGroup {
    pub id: String,
    pub title: String,
    pub pane_root: SavedPaneLayout,
    #[serde(default)]
    pub tabs: Vec<SavedTerminalTab>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedTabsState {
    #[serde(default)]
    pub groups: Vec<SavedTabGroup>,
    #[serde(default)]
    pub active_group: Option<String>,
    #[serde(default)]
    pub active_tab: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TitleBarStyle {
    Native,
    #[default]
    Integrated,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum CursorStyle {
    #[default]
    Default,
    Blink,
    Beam,
    BeamBlink,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigFile {
    #[serde(default = "default_follow_system_theme")]
    pub follow_system_theme: bool,
    #[serde(default)]
    pub theme_mode: String,
    #[serde(default)]
    pub light_theme_name: String,
    #[serde(default)]
    pub dark_theme_name: String,
    #[serde(default = "default_locale")]
    pub locale: String,
    #[serde(default = "default_terminal_font_size")]
    pub terminal_font_size: f32,
    #[serde(default = "default_ui_font_size")]
    pub ui_font_size: f32,
    #[serde(default)]
    pub right_click_copy_paste: bool,
    #[serde(default)]
    pub keyword_highlight: bool,
    #[serde(default = "default_ui_font_family")]
    pub ui_font_family: String,
    #[serde(default = "default_terminal_font_family")]
    pub terminal_font_family: String,
    #[serde(default)]
    pub title_bar_style: TitleBarStyle,
    #[serde(default)]
    pub cursor_style: CursorStyle,
    #[serde(default)]
    pub sessions: Vec<Session>,
    /// Shell commands recorded for each SSH session ID.
    #[serde(default)]
    pub command_history: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub command_history_revision: u64,
    #[serde(default)]
    pub remember_tabs: bool,
    #[serde(default)]
    pub saved_tabs: Option<SavedTabsState>,
    #[serde(default)]
    pub window_bounds: Option<SavedWindowBounds>,
    #[serde(default)]
    pub workspace_panels: Option<Vec<f32>>,
    #[serde(default)]
    pub body_panels: Option<Vec<f32>>,
    #[serde(default)]
    pub sftp_tree_panels: Option<Vec<f32>>,
    #[serde(default)]
    pub sftp_file_columns: Option<Vec<f32>>,
    #[serde(default)]
    pub sftp_file_columns_customized: bool,
    #[serde(default)]
    pub transfers: Vec<crate::terminal::Transfer>,
    #[serde(default)]
    pub show_hidden_files: bool,
    #[serde(default)]
    pub lock_layout: bool,
    #[serde(default = "default_monitoring_position")]
    pub monitoring_position: String,
    #[serde(default)]
    pub sidebar_collapsed: bool,
    #[serde(default)]
    pub sftp_panel_minimized: bool,
    #[serde(default)]
    pub key_bindings: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub sync_endpoint: String,
    #[serde(default)]
    pub sync_username: String,
    #[serde(default)]
    pub sync_etag: Option<String>,
    #[serde(default)]
    pub sync_device_id: String,
    #[serde(default)]
    pub sync_backend: String,
    #[serde(default)]
    pub sync_etag_backend: String,
    #[serde(default)]
    pub sync_s3_endpoint: String,
    #[serde(default = "default_s3_region")]
    pub sync_s3_region: String,
    #[serde(default)]
    pub sync_s3_bucket: String,
    #[serde(default = "default_s3_object_key")]
    pub sync_s3_object_key: String,
    #[serde(default)]
    pub use_proxy: bool,
    #[serde(default = "default_read_env_proxy")]
    pub read_env_proxy: bool,
    #[serde(default = "default_global_proxy_type")]
    pub global_proxy_type: String,
    #[serde(default)]
    pub global_proxy_host: String,
    #[serde(default)]
    pub global_proxy_port: Option<u16>,
    #[serde(default)]
    pub global_proxy_user: String,
    #[serde(default)]
    pub global_proxy_password: String,
}

fn default_read_env_proxy() -> bool {
    true
}

fn default_global_proxy_type() -> String {
    "socks5".to_string()
}

fn default_monitoring_position() -> String {
    "Sidebar".to_string()
}

fn default_s3_region() -> String {
    "us-east-1".to_string()
}

fn default_s3_object_key() -> String {
    "ashell-sync.json".to_string()
}

fn default_follow_system_theme() -> bool {
    true
}

fn default_locale() -> String {
    "system".to_string()
}

fn default_terminal_font_size() -> f32 {
    18.0
}

fn default_ui_font_size() -> f32 {
    14.0
}

pub fn default_ui_font_family() -> String {
    // ".SystemUIFont" is a GPUI sentinel that resolves to the platform system UI font.
    // This matches gpui-component's own Theme default.
    ".SystemUIFont".to_string()
}

fn default_terminal_font_family() -> String {
    "Maple Mono NF CN".to_string()
}

const MAX_COMMAND_HISTORY: usize = 200;

fn normalize_command_history_entries(history: &mut Vec<String>) -> bool {
    let mut seen = HashSet::new();
    let mut changed = false;
    let mut normalized = history
        .drain(..)
        .rev()
        .filter_map(|command| {
            let trimmed = command.trim();
            changed |= trimmed.len() != command.len();
            let command = trimmed.to_string();
            if command.is_empty() || !seen.insert(command.clone()) {
                changed = true;
                None
            } else {
                Some(command)
            }
        })
        .collect::<Vec<_>>();
    normalized.reverse();
    if normalized.len() > MAX_COMMAND_HISTORY {
        changed = true;
        normalized.drain(..normalized.len() - MAX_COMMAND_HISTORY);
    }
    *history = normalized;
    changed
}

impl Default for ConfigFile {
    fn default() -> Self {
        Self {
            follow_system_theme: default_follow_system_theme(),
            theme_mode: String::new(),
            light_theme_name: String::new(),
            dark_theme_name: String::new(),
            locale: default_locale(),
            terminal_font_size: default_terminal_font_size(),
            ui_font_size: default_ui_font_size(),
            right_click_copy_paste: false,
            keyword_highlight: false,
            ui_font_family: default_ui_font_family(),
            terminal_font_family: default_terminal_font_family(),
            title_bar_style: TitleBarStyle::default(),
            cursor_style: CursorStyle::default(),
            sessions: Vec::new(),
            command_history: HashMap::new(),
            command_history_revision: 0,
            remember_tabs: false,
            saved_tabs: None,
            window_bounds: None,
            workspace_panels: None,
            body_panels: None,
            sftp_tree_panels: None,
            sftp_file_columns: None,
            sftp_file_columns_customized: false,
            transfers: Vec::new(),
            show_hidden_files: false,
            lock_layout: false,
            monitoring_position: default_monitoring_position(),
            sidebar_collapsed: false,
            sftp_panel_minimized: false,
            key_bindings: std::collections::HashMap::new(),
            sync_endpoint: String::new(),
            sync_username: String::new(),
            sync_etag: None,
            sync_device_id: String::new(),
            sync_backend: String::new(),
            sync_etag_backend: String::new(),
            sync_s3_endpoint: String::new(),
            sync_s3_region: default_s3_region(),
            sync_s3_bucket: String::new(),
            sync_s3_object_key: default_s3_object_key(),
            use_proxy: false,
            read_env_proxy: true,
            global_proxy_type: default_global_proxy_type(),
            global_proxy_host: String::new(),
            global_proxy_port: None,
            global_proxy_user: String::new(),
            global_proxy_password: String::new(),
        }
    }
}

#[derive(Clone)]
pub struct ConfigStore {
    pub(crate) path: PathBuf,
    pub(crate) cache: ConfigFile,
    write_lock: Arc<Mutex<()>>,
}

fn config_backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

fn decode_config_bytes(raw_bytes: &[u8], hardware_uuid: &str) -> Result<ConfigFile> {
    match decrypt_config(raw_bytes, hardware_uuid) {
        Ok(cache) => Ok(cache),
        Err(decrypt_err) => serde_json::from_slice::<ConfigFile>(raw_bytes).map_err(|json_err| {
            anyhow::anyhow!(
                "decrypt failed: {decrypt_err:#}; plain JSON parsing failed: {json_err:#}"
            )
        }),
    }
}

fn persist_config_bytes(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("configuration path has no parent directory")?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".ashell-config-")
        .suffix(".tmp")
        .tempfile_in(parent)
        .with_context(|| format!("failed to create temporary config in {}", parent.display()))?;
    temporary
        .write_all(contents)
        .with_context(|| format!("failed to write temporary config for {}", path.display()))?;
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("failed to sync temporary config for {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let mut permissions = temporary
            .as_file()
            .metadata()
            .with_context(|| format!("failed to inspect temporary config for {}", path.display()))?
            .permissions();
        permissions.set_mode(0o600);
        temporary
            .as_file()
            .set_permissions(permissions)
            .with_context(|| {
                format!("failed to protect temporary config for {}", path.display())
            })?;
    }

    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to replace {} atomically", path.display()))?;

    #[cfg(unix)]
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("failed to sync config directory {}", parent.display()))?;

    Ok(())
}

fn write_config_bytes(path: &Path, contents: &[u8]) -> Result<()> {
    if path.exists() {
        let previous = fs::read(path)
            .with_context(|| format!("failed to read previous config {}", path.display()))?;
        persist_config_bytes(&config_backup_path(path), &previous)
            .with_context(|| format!("failed to back up config {}", path.display()))?;
    }
    persist_config_bytes(path, contents)
}

impl ConfigStore {
    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create config dir {}", parent.display()))?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(mut perms) = fs::metadata(parent).map(|m| m.permissions()) {
                    perms.set_mode(0o700);
                    let _ = fs::set_permissions(parent, perms);
                }
            }

            let tmp_dir = parent.join("tmp");
            let _ = fs::remove_dir_all(&tmp_dir);
            let _ = fs::create_dir_all(&tmp_dir);
        }

        let mut cache = if path.exists() {
            let raw_bytes =
                fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
            let hardware_uuid = get_hardware_uuid();
            match decode_config_bytes(&raw_bytes, &hardware_uuid) {
                Ok(mut cache) => {
                    let backup_path = config_backup_path(&path);
                    if let Ok(backup_bytes) = fs::read(&backup_path)
                        && let Ok(backup_cache) = decode_config_bytes(&backup_bytes, &hardware_uuid)
                    {
                        let backup_history_is_newer = backup_cache.command_history_revision
                            > cache.command_history_revision
                            || (backup_cache.command_history_revision == 0
                                && cache.command_history_revision == 0
                                && cache.command_history.is_empty()
                                && !backup_cache.command_history.is_empty());
                        if backup_history_is_newer {
                            cache.command_history = backup_cache.command_history;
                            cache.command_history_revision =
                                backup_cache.command_history_revision.max(1);
                            let encrypted_bytes = encrypt_config(&cache, &hardware_uuid)?;
                            persist_config_bytes(&path, &encrypted_bytes).with_context(|| {
                                format!(
                                    "failed to restore command history in {} from {}",
                                    path.display(),
                                    backup_path.display()
                                )
                            })?;
                            tracing::warn!(
                                "restored newer command history in {} from {}",
                                path.display(),
                                backup_path.display(),
                            );
                        }
                    }
                    cache
                }
                Err(primary_err) => {
                    let backup_path = config_backup_path(&path);
                    let backup_bytes = fs::read(&backup_path).with_context(|| {
                        format!("failed to read config backup {}", backup_path.display())
                    });
                    match backup_bytes.and_then(|backup_bytes| {
                        decode_config_bytes(&backup_bytes, &hardware_uuid)
                            .map(|cache| (backup_bytes, cache))
                    }) {
                        Ok((backup_bytes, cache)) => {
                            tracing::warn!(
                                "failed to load config {}: {primary_err:#}; restored {}",
                                path.display(),
                                backup_path.display(),
                            );
                            persist_config_bytes(&path, &backup_bytes).with_context(|| {
                                format!(
                                    "failed to restore config {} from {}",
                                    path.display(),
                                    backup_path.display()
                                )
                            })?;
                            cache
                        }
                        Err(backup_err) => {
                            return Err(anyhow::anyhow!(
                                "failed to load config {}: {primary_err:#}; backup recovery failed: {backup_err:#}",
                                path.display()
                            ));
                        }
                    }
                }
            }
        } else {
            ConfigFile::default()
        };

        if cache.sync_device_id.is_empty() {
            cache.sync_device_id = Uuid::new_v4().to_string();
        }
        let mut history_changed = false;
        for history in cache.command_history.values_mut() {
            history_changed |= normalize_command_history_entries(history);
        }
        let previous_history_count = cache.command_history.len();
        cache
            .command_history
            .retain(|_, history| !history.is_empty());
        history_changed |= cache.command_history.len() != previous_history_count;
        if history_changed {
            cache.command_history_revision = cache.command_history_revision.saturating_add(1);
        }
        Ok(Self {
            path,
            cache,
            write_lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn in_memory() -> Self {
        let cache = ConfigFile {
            sync_device_id: Uuid::new_v4().to_string(),
            ..ConfigFile::default()
        };
        Self {
            path: PathBuf::new(),
            cache,
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    fn config_path() -> Result<PathBuf> {
        let dirs = BaseDirs::new().context("could not determine user home directory")?;
        Ok(dirs
            .home_dir()
            .join(".config")
            .join("ashell")
            .join("sessions.json"))
    }

    pub fn sessions(&self) -> &[Session] {
        &self.cache.sessions
    }

    pub fn replace_sessions(&mut self, sessions: Vec<Session>) {
        self.cache.sessions = sessions;
    }

    /// Return persisted command history for all SSH sessions, newest first per session.
    pub fn all_command_history(&self) -> Vec<(String, usize, String)> {
        let mut session_ids = self
            .cache
            .command_history
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        session_ids.sort();

        let mut entries = Vec::new();
        for session_id in session_ids {
            if let Some(history) = self.cache.command_history.get(&session_id) {
                entries.extend(
                    history
                        .iter()
                        .enumerate()
                        .rev()
                        .map(|(index, command)| (session_id.clone(), index, command.clone())),
                );
            }
        }
        entries
    }

    pub fn add_command_history(&mut self, session_id: &str, command: String) -> bool {
        let command = command.trim().to_string();
        if command.is_empty() {
            return false;
        }

        let history = self
            .cache
            .command_history
            .entry(session_id.to_string())
            .or_default();
        let was_latest = history.last().is_some_and(|previous| previous == &command);
        let previous_len = history.len();
        history.retain(|previous| previous != &command);
        history.push(command);
        if history.len() > MAX_COMMAND_HISTORY {
            let excess = history.len() - MAX_COMMAND_HISTORY;
            history.drain(..excess);
        }
        let changed = !was_latest || history.len() != previous_len;
        if changed {
            self.cache.command_history_revision =
                self.cache.command_history_revision.saturating_add(1);
        }
        changed
    }

    pub fn normalize_command_history(&mut self) {
        let mut changed = false;
        for history in self.cache.command_history.values_mut() {
            changed |= normalize_command_history_entries(history);
        }
        let previous_history_count = self.cache.command_history.len();
        self.cache
            .command_history
            .retain(|_, history| !history.is_empty());
        changed |= self.cache.command_history.len() != previous_history_count;
        if changed {
            self.cache.command_history_revision =
                self.cache.command_history_revision.saturating_add(1);
        }
    }

    pub fn remove_command_history(&mut self, session_id: &str, index: usize) -> bool {
        let became_empty = {
            let Some(history) = self.cache.command_history.get_mut(session_id) else {
                return false;
            };
            if index >= history.len() {
                return false;
            }
            history.remove(index);
            history.is_empty()
        };
        if became_empty {
            self.cache.command_history.remove(session_id);
        }
        self.cache.command_history_revision = self.cache.command_history_revision.saturating_add(1);
        true
    }

    pub fn sync_endpoint(&self) -> &str {
        &self.cache.sync_endpoint
    }

    pub fn sync_username(&self) -> &str {
        &self.cache.sync_username
    }

    pub fn sync_etag(&self) -> Option<&str> {
        (self.cache.sync_etag_backend == self.sync_backend())
            .then_some(self.cache.sync_etag.as_deref())
            .flatten()
    }

    pub fn sync_device_id(&self) -> &str {
        &self.cache.sync_device_id
    }

    pub fn sync_backend(&self) -> &str {
        if self.cache.sync_backend == "s3" {
            "s3"
        } else {
            "webdav"
        }
    }

    pub fn set_sync_backend(&mut self, backend: &str) {
        self.cache.sync_backend = if backend == "s3" { "s3" } else { "webdav" }.to_string();
    }

    pub fn sync_s3_endpoint(&self) -> &str {
        &self.cache.sync_s3_endpoint
    }

    pub fn sync_s3_region(&self) -> &str {
        if self.cache.sync_s3_region.is_empty() {
            "us-east-1"
        } else {
            &self.cache.sync_s3_region
        }
    }

    pub fn sync_s3_bucket(&self) -> &str {
        &self.cache.sync_s3_bucket
    }

    pub fn sync_s3_object_key(&self) -> &str {
        if self.cache.sync_s3_object_key.is_empty() {
            "ashell-sync.json"
        } else {
            &self.cache.sync_s3_object_key
        }
    }

    pub fn set_sync_connection(&mut self, endpoint: String, username: String) {
        self.cache.sync_endpoint = endpoint;
        self.cache.sync_username = username;
    }

    pub fn set_sync_s3_connection(
        &mut self,
        endpoint: String,
        region: String,
        bucket: String,
        object_key: String,
    ) {
        self.cache.sync_s3_endpoint = endpoint;
        self.cache.sync_s3_region = region;
        self.cache.sync_s3_bucket = bucket;
        self.cache.sync_s3_object_key = object_key;
    }

    pub fn set_sync_etag(&mut self, etag: Option<String>) {
        self.cache.sync_etag = etag;
        self.cache.sync_etag_backend = self.sync_backend().to_string();
    }

    pub fn follow_system_theme(&self) -> bool {
        self.cache.follow_system_theme
    }

    pub fn theme_mode(&self) -> &str {
        &self.cache.theme_mode
    }

    pub fn light_theme_name(&self) -> &str {
        &self.cache.light_theme_name
    }

    pub fn dark_theme_name(&self) -> &str {
        &self.cache.dark_theme_name
    }

    pub fn locale(&self) -> &str {
        if self.cache.locale.is_empty() {
            "system"
        } else {
            &self.cache.locale
        }
    }

    pub fn set_locale(&mut self, locale: &str) {
        self.cache.locale = locale.to_string();
    }

    pub fn key_bindings(&self) -> &std::collections::HashMap<String, String> {
        &self.cache.key_bindings
    }

    pub fn set_key_binding(&mut self, action_name: &str, keystroke: &str) {
        self.cache
            .key_bindings
            .insert(action_name.to_string(), keystroke.to_string());
    }

    pub fn monitoring_position(&self) -> &str {
        if self.cache.monitoring_position.is_empty() {
            "Sidebar"
        } else {
            &self.cache.monitoring_position
        }
    }

    pub fn set_monitoring_position(&mut self, pos: &str) {
        self.cache.monitoring_position = pos.to_string();
    }

    pub fn terminal_font_size(&self) -> f32 {
        if self.cache.terminal_font_size <= 0.0 {
            default_terminal_font_size()
        } else {
            self.cache.terminal_font_size
        }
    }

    pub fn set_theme_preferences(
        &mut self,
        follow_system_theme: bool,
        theme_mode: impl Into<String>,
        light_theme_name: impl Into<String>,
        dark_theme_name: impl Into<String>,
    ) {
        self.cache.follow_system_theme = follow_system_theme;
        self.cache.theme_mode = theme_mode.into();
        self.cache.light_theme_name = light_theme_name.into();
        self.cache.dark_theme_name = dark_theme_name.into();
    }

    pub fn window_bounds(&self) -> Option<&SavedWindowBounds> {
        self.cache.window_bounds.as_ref()
    }

    pub fn remember_tabs(&self) -> bool {
        self.cache.remember_tabs
    }

    pub fn set_remember_tabs(&mut self, remember_tabs: bool) {
        self.cache.remember_tabs = remember_tabs;
        if !remember_tabs {
            self.cache.saved_tabs = None;
        }
    }

    pub fn saved_tabs(&self) -> Option<&SavedTabsState> {
        self.cache.saved_tabs.as_ref()
    }

    pub fn set_saved_tabs(&mut self, saved_tabs: Option<SavedTabsState>) {
        self.cache.saved_tabs = saved_tabs;
    }

    pub fn workspace_panels(&self) -> Option<&Vec<f32>> {
        self.cache.workspace_panels.as_ref()
    }

    #[allow(dead_code)]
    pub fn body_panels(&self) -> Option<&Vec<f32>> {
        self.cache.body_panels.as_ref()
    }

    pub fn sftp_tree_panels(&self) -> Option<&Vec<f32>> {
        self.cache.sftp_tree_panels.as_ref()
    }

    pub fn sftp_file_columns(&self) -> Option<&Vec<f32>> {
        self.cache.sftp_file_columns.as_ref()
    }

    pub fn sftp_file_columns_customized(&self) -> bool {
        self.cache.sftp_file_columns_customized
    }

    pub fn transfers(&self) -> Vec<crate::terminal::Transfer> {
        self.cache.transfers.clone()
    }

    pub fn set_transfers(&mut self, transfers: Vec<crate::terminal::Transfer>) {
        self.cache.transfers = transfers;
        if let Err(err) = self.save() {
            tracing::error!("failed to save config: {err:#}");
        }
    }

    pub fn set_layout_state(
        &mut self,
        window_bounds: Option<SavedWindowBounds>,
        workspace_panels: Option<Vec<f32>>,
        body_panels: Option<Vec<f32>>,
    ) {
        self.cache.window_bounds = window_bounds;
        self.cache.workspace_panels = workspace_panels;
        self.cache.body_panels = body_panels;
    }

    pub fn set_sftp_tree_panels(&mut self, panels: Option<Vec<f32>>) {
        self.cache.sftp_tree_panels = panels;
    }

    pub fn set_sftp_file_columns(&mut self, columns: Option<Vec<f32>>) {
        self.cache.sftp_file_columns = columns;
    }

    pub fn set_sftp_file_columns_customized(&mut self, customized: bool) {
        self.cache.sftp_file_columns_customized = customized;
    }

    pub fn set_terminal_font_size(&mut self, terminal_font_size: f32) {
        self.cache.terminal_font_size = terminal_font_size.max(10.0);
    }

    pub fn ui_font_size(&self) -> f32 {
        if self.cache.ui_font_size <= 0.0 {
            default_ui_font_size()
        } else {
            self.cache.ui_font_size
        }
    }

    pub fn set_ui_font_size(&mut self, ui_font_size: f32) {
        self.cache.ui_font_size = ui_font_size.max(8.0);
    }

    pub fn ui_font_family(&self) -> &str {
        if self.cache.ui_font_family.is_empty() {
            ".SystemUIFont"
        } else {
            &self.cache.ui_font_family
        }
    }

    pub fn set_ui_font_family(&mut self, family: &str) {
        self.cache.ui_font_family = family.to_string();
    }

    pub fn right_click_copy_paste(&self) -> bool {
        self.cache.right_click_copy_paste
    }

    pub fn set_right_click_copy_paste(&mut self, val: bool) {
        self.cache.right_click_copy_paste = val;
    }

    pub fn keyword_highlight(&self) -> bool {
        self.cache.keyword_highlight
    }

    pub fn set_keyword_highlight(&mut self, val: bool) {
        self.cache.keyword_highlight = val;
    }

    pub fn terminal_font_family(&self) -> &str {
        if self.cache.terminal_font_family.is_empty() {
            "Maple Mono NF CN"
        } else {
            &self.cache.terminal_font_family
        }
    }

    pub fn set_terminal_font_family(&mut self, family: &str) {
        self.cache.terminal_font_family = family.to_string();
    }

    pub fn title_bar_style(&self) -> TitleBarStyle {
        self.cache.title_bar_style
    }

    pub fn set_title_bar_style(&mut self, style: TitleBarStyle) {
        self.cache.title_bar_style = style;
    }

    pub fn cursor_style(&self) -> CursorStyle {
        self.cache.cursor_style
    }

    pub fn set_cursor_style(&mut self, style: CursorStyle) {
        self.cache.cursor_style = style;
    }

    pub fn use_proxy(&self) -> bool {
        self.cache.use_proxy
    }
    pub fn set_use_proxy(&mut self, val: bool) {
        self.cache.use_proxy = val;
    }
    pub fn read_env_proxy(&self) -> bool {
        self.cache.read_env_proxy
    }
    pub fn set_read_env_proxy(&mut self, val: bool) {
        self.cache.read_env_proxy = val;
    }
    pub fn global_proxy_type(&self) -> &str {
        &self.cache.global_proxy_type
    }
    pub fn set_global_proxy_type(&mut self, val: String) {
        self.cache.global_proxy_type = val;
    }
    pub fn global_proxy_host(&self) -> &str {
        &self.cache.global_proxy_host
    }
    pub fn set_global_proxy_host(&mut self, val: String) {
        self.cache.global_proxy_host = val;
    }
    pub fn global_proxy_port(&self) -> Option<u16> {
        self.cache.global_proxy_port
    }
    pub fn set_global_proxy_port(&mut self, val: Option<u16>) {
        self.cache.global_proxy_port = val;
    }
    pub fn global_proxy_user(&self) -> &str {
        &self.cache.global_proxy_user
    }
    pub fn set_global_proxy_user(&mut self, val: String) {
        self.cache.global_proxy_user = val;
    }
    pub fn global_proxy_password(&self) -> &str {
        &self.cache.global_proxy_password
    }
    pub fn set_global_proxy_password(&mut self, val: String) {
        self.cache.global_proxy_password = val;
    }

    pub fn show_hidden_files(&self) -> bool {
        self.cache.show_hidden_files
    }

    pub fn set_show_hidden_files(&mut self, val: bool) {
        self.cache.show_hidden_files = val;
    }

    pub fn lock_layout(&self) -> bool {
        self.cache.lock_layout
    }

    pub fn set_lock_layout(&mut self, val: bool) {
        self.cache.lock_layout = val;
    }

    pub fn sidebar_collapsed(&self) -> bool {
        self.cache.sidebar_collapsed
    }

    pub fn set_sidebar_collapsed(&mut self, val: bool) {
        self.cache.sidebar_collapsed = val;
    }

    pub fn sftp_panel_minimized(&self) -> bool {
        self.cache.sftp_panel_minimized
    }

    pub fn set_sftp_panel_minimized(&mut self, val: bool) {
        self.cache.sftp_panel_minimized = val;
    }

    pub fn get(&self, id: &str) -> Option<&Session> {
        self.cache.sessions.iter().find(|s| s.id == id)
    }

    pub fn upsert(&mut self, session: Session) {
        if let Some(existing) = self.cache.sessions.iter_mut().find(|s| s.id == session.id) {
            *existing = session;
        } else {
            self.cache.sessions.push(session);
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.cache.sessions.retain(|s| s.id != id);
        if self.cache.command_history.remove(id).is_some() {
            self.cache.command_history_revision =
                self.cache.command_history_revision.saturating_add(1);
        }
    }

    pub fn save(&self) -> Result<()> {
        if self.path.as_os_str().is_empty() {
            return Ok(());
        }
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("configuration write lock is poisoned"))?;
        let hardware_uuid = get_hardware_uuid();
        let encrypted_bytes = encrypt_config(&self.cache, &hardware_uuid)?;
        write_config_bytes(&self.path, &encrypted_bytes)
    }

    pub fn save_merged_preferences(&self, local_config: ConfigFile) -> Result<()> {
        if self.path.as_os_str().is_empty() {
            return Ok(());
        }
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("configuration write lock is poisoned"))?;
        let hardware_uuid = get_hardware_uuid();

        let mut disk_config = if self.path.exists() {
            let raw_bytes = fs::read(&self.path)
                .with_context(|| format!("failed to read {}", self.path.display()))?;
            decode_config_bytes(&raw_bytes, &hardware_uuid).with_context(|| {
                format!(
                    "refusing to overwrite unreadable config {}",
                    self.path.display()
                )
            })?
        } else {
            self.cache.clone()
        };

        // Merge UI preference fields
        disk_config.follow_system_theme = local_config.follow_system_theme;
        disk_config.theme_mode = local_config.theme_mode;
        disk_config.light_theme_name = local_config.light_theme_name;
        disk_config.dark_theme_name = local_config.dark_theme_name;
        disk_config.locale = local_config.locale;
        disk_config.terminal_font_size = local_config.terminal_font_size;
        disk_config.ui_font_size = local_config.ui_font_size;
        disk_config.right_click_copy_paste = local_config.right_click_copy_paste;
        disk_config.keyword_highlight = local_config.keyword_highlight;
        disk_config.ui_font_family = local_config.ui_font_family;
        disk_config.terminal_font_family = local_config.terminal_font_family;
        disk_config.title_bar_style = local_config.title_bar_style;
        disk_config.cursor_style = local_config.cursor_style;
        if local_config.command_history_revision >= disk_config.command_history_revision {
            disk_config.command_history = local_config.command_history;
            disk_config.command_history_revision = local_config.command_history_revision;
        }
        disk_config.remember_tabs = local_config.remember_tabs;
        disk_config.saved_tabs = local_config.saved_tabs;
        disk_config.window_bounds = local_config.window_bounds;
        disk_config.workspace_panels = local_config.workspace_panels;
        disk_config.body_panels = local_config.body_panels;
        disk_config.sftp_tree_panels = local_config.sftp_tree_panels;
        disk_config.sftp_file_columns = local_config.sftp_file_columns;
        disk_config.sftp_file_columns_customized = local_config.sftp_file_columns_customized;
        disk_config.show_hidden_files = local_config.show_hidden_files;
        disk_config.lock_layout = local_config.lock_layout;
        disk_config.monitoring_position = local_config.monitoring_position;
        disk_config.sidebar_collapsed = local_config.sidebar_collapsed;
        disk_config.sftp_panel_minimized = local_config.sftp_panel_minimized;
        disk_config.key_bindings = local_config.key_bindings;
        disk_config.use_proxy = local_config.use_proxy;
        disk_config.read_env_proxy = local_config.read_env_proxy;
        disk_config.global_proxy_type = local_config.global_proxy_type;
        disk_config.global_proxy_host = local_config.global_proxy_host;
        disk_config.global_proxy_port = local_config.global_proxy_port;
        disk_config.global_proxy_user = local_config.global_proxy_user;
        disk_config.global_proxy_password = local_config.global_proxy_password;

        let encrypted_bytes = encrypt_config(&disk_config, &hardware_uuid)?;
        write_config_bytes(&self.path, &encrypted_bytes)
    }
}

pub trait ProxyStream:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + Sync + 'static
{
}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + Sync + 'static> ProxyStream
    for T
{
}

#[derive(Debug, Clone)]
pub struct EnvProxy {
    pub proxy_type: String,
    pub host: String,
    pub port: Option<u16>,
    pub user: String,
    pub pass: String,
}

pub static ENV_PROXY: OnceLock<Option<EnvProxy>> = OnceLock::new();

pub async fn connect_proxy(session: &Session) -> Result<Box<dyn ProxyStream>> {
    let target_host = session.host.clone();
    let target_port = session.port;
    let session = session.clone();

    let connect_fut = async move {
        let target_host = &target_host;
        let config = ConfigStore::load().unwrap_or_else(|_| ConfigStore::in_memory());
        let (proxy_type, proxy_host, proxy_port, proxy_user, proxy_password) = {
            if !session.proxy_type.is_empty() && session.proxy_type != "none" {
                (
                    session.proxy_type.clone(),
                    session.proxy_host.clone(),
                    session.proxy_port,
                    session.proxy_user.clone(),
                    session.proxy_password.clone(),
                )
            } else if config.cache.read_env_proxy
                && ENV_PROXY.get().and_then(|opt| opt.as_ref()).is_some()
            {
                let env_p = ENV_PROXY.get().and_then(|opt| opt.as_ref()).unwrap();
                (
                    env_p.proxy_type.clone(),
                    env_p.host.clone(),
                    env_p.port,
                    env_p.user.clone(),
                    env_p.pass.clone(),
                )
            } else if config.cache.use_proxy {
                (
                    config.cache.global_proxy_type.clone(),
                    config.cache.global_proxy_host.clone(),
                    config.cache.global_proxy_port,
                    config.cache.global_proxy_user.clone(),
                    config.cache.global_proxy_password.clone(),
                )
            } else {
                (
                    "none".to_string(),
                    String::new(),
                    None,
                    String::new(),
                    String::new(),
                )
            }
        };

        if proxy_type != "none" && (proxy_host.is_empty() || proxy_port.is_none()) {
            let addr = format!("{}:{}", target_host, target_port);
            let stream = tokio::net::TcpStream::connect(&addr).await?;
            return Ok(Box::new(stream) as Box<dyn ProxyStream>);
        }

        match proxy_type.as_str() {
            "socks5" | "socks5h" => {
                let proxy_port = proxy_port.unwrap_or(1080);
                let proxy_addr = format!("{}:{}", proxy_host, proxy_port);

                if !proxy_user.is_empty() {
                    let stream = tokio_socks::tcp::Socks5Stream::connect_with_password(
                        proxy_addr.as_str(),
                        (target_host.as_str(), target_port),
                        &proxy_user,
                        &proxy_password,
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("SOCKS5 proxy connection failed: {}", e))?;
                    Ok(Box::new(stream) as Box<dyn ProxyStream>)
                } else {
                    let stream = tokio_socks::tcp::Socks5Stream::connect(
                        proxy_addr.as_str(),
                        (target_host.as_str(), target_port),
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("SOCKS5 proxy connection failed: {}", e))?;
                    Ok(Box::new(stream) as Box<dyn ProxyStream>)
                }
            }
            "http" => {
                let proxy_port = proxy_port.unwrap_or(8080);
                let proxy_addr = format!("{}:{}", proxy_host, proxy_port);

                use tokio::io::AsyncWriteExt;
                let mut stream = tokio::net::TcpStream::connect(&proxy_addr)
                    .await
                    .map_err(|e| anyhow::anyhow!("HTTP proxy connection failed: {}", e))?;

                let mut request = format!(
                    "CONNECT {}:{} HTTP/1.1\r\nHost: {}:{}\r\n",
                    target_host, target_port, target_host, target_port
                );
                if !proxy_user.is_empty() {
                    use base64::Engine as _;
                    let auth = format!("{}:{}", proxy_user, proxy_password);
                    let encoded = base64::engine::general_purpose::STANDARD.encode(auth);
                    request.push_str(&format!("Proxy-Authorization: Basic {}\r\n", encoded));
                }
                request.push_str("\r\n");

                stream.write_all(request.as_bytes()).await?;

                let mut response = [0u8; 1024];
                let n = tokio::io::AsyncReadExt::read(&mut stream, &mut response).await?;
                let resp_str = String::from_utf8_lossy(&response[..n]);
                if !resp_str.contains("200") && !resp_str.contains("established") {
                    return Err(anyhow::anyhow!("HTTP proxy CONNECT failed: {}", resp_str));
                }

                Ok(Box::new(stream) as Box<dyn ProxyStream>)
            }
            _ => {
                let addr = format!("{}:{}", target_host, target_port);
                let stream = tokio::net::TcpStream::connect(&addr).await?;
                Ok(Box::new(stream) as Box<dyn ProxyStream>)
            }
        }
    };

    tokio::time::timeout(std::time::Duration::from_secs(16), connect_fut)
        .await
        .map_err(|_| anyhow::anyhow!("connection timed out after 16 seconds"))?
}

pub fn active_proxy(session: &Session) -> Option<(String, String, Option<u16>)> {
    let config = ConfigStore::load().unwrap_or_else(|_| ConfigStore::in_memory());
    let (proxy_type, proxy_host, proxy_port, _, _) = {
        if !session.proxy_type.is_empty() && session.proxy_type != "none" {
            (
                session.proxy_type.clone(),
                session.proxy_host.clone(),
                session.proxy_port,
                session.proxy_user.clone(),
                session.proxy_password.clone(),
            )
        } else if config.cache.read_env_proxy
            && ENV_PROXY.get().and_then(|opt| opt.as_ref()).is_some()
        {
            let env_p = ENV_PROXY.get().and_then(|opt| opt.as_ref()).unwrap();
            (
                env_p.proxy_type.clone(),
                env_p.host.clone(),
                env_p.port,
                env_p.user.clone(),
                env_p.pass.clone(),
            )
        } else if config.cache.use_proxy {
            (
                config.cache.global_proxy_type.clone(),
                config.cache.global_proxy_host.clone(),
                config.cache.global_proxy_port,
                config.cache.global_proxy_user.clone(),
                config.cache.global_proxy_password.clone(),
            )
        } else {
            (
                "none".to_string(),
                String::new(),
                None,
                String::new(),
                String::new(),
            )
        }
    };

    if proxy_type != "none" && !proxy_host.is_empty() && proxy_port.is_some() {
        Some((proxy_type, proxy_host, proxy_port))
    } else {
        None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EncryptedConfigEnvelope {
    format_version: u32,
    kdf: String,
    cipher: String,
    salt: String,
    nonce: String,
    payload: String,
}

static HARDWARE_UUID_CACHE: OnceLock<String> = OnceLock::new();

pub fn get_hardware_uuid() -> String {
    HARDWARE_UUID_CACHE
        .get_or_init(|| {
            #[cfg(target_os = "macos")]
            {
                if let Ok(output) = std::process::Command::new("ioreg")
                    .args(["-rd1", "-c", "IOPlatformExpertDevice"])
                    .output()
                {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    for line in stdout.lines() {
                        if line.contains("IOPlatformUUID") {
                            if let Some(uuid) = line.split('"').nth(3) {
                                let uuid = uuid.trim().to_string();
                                if !uuid.is_empty() {
                                    return uuid;
                                }
                            }
                        }
                    }
                }
            }

            #[cfg(target_os = "linux")]
            {
                if let Ok(uuid) = std::fs::read_to_string("/sys/class/dmi/id/product_uuid") {
                    let uuid = uuid.trim().to_string();
                    if !uuid.is_empty() {
                        return uuid;
                    }
                }
                if let Ok(id) = std::fs::read_to_string("/etc/machine-id") {
                    let id = id.trim().to_string();
                    if !id.is_empty() {
                        return id;
                    }
                }
                if let Ok(id) = std::fs::read_to_string("/var/lib/dbus/machine-id") {
                    let id = id.trim().to_string();
                    if !id.is_empty() {
                        return id;
                    }
                }
            }

            #[cfg(target_os = "windows")]
            {
                use winreg::RegKey;
                use winreg::enums::HKEY_LOCAL_MACHINE;
                let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
                if let Ok(subkey) = hklm.open_subkey("SOFTWARE\\Microsoft\\Cryptography") {
                    if let Ok(guid) = subkey.get_value::<String, _>("MachineGuid") {
                        let guid = guid.trim().to_string();
                        if !guid.is_empty() {
                            return guid;
                        }
                    }
                }
            }

            "ashell-default-hardware-uuid-fallback".to_string()
        })
        .clone()
}

fn encrypt_config(config: &ConfigFile, password: &str) -> Result<Vec<u8>> {
    let mut salt = [0u8; 16];
    let mut nonce = [0u8; 24];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut nonce);

    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(password.as_bytes(), &salt, &mut key)
        .map_err(|err| anyhow::anyhow!("derive encryption key: {err}"))?;

    let plaintext = serde_json::to_vec(config).context("serialize config")?;
    let ciphertext = XChaCha20Poly1305::new((&key).into())
        .encrypt(XNonce::from_slice(&nonce), plaintext.as_ref())
        .map_err(|_| anyhow::anyhow!("encrypt config payload"))?;

    serde_json::to_vec_pretty(&EncryptedConfigEnvelope {
        format_version: 1,
        kdf: "argon2id".to_string(),
        cipher: "xchacha20poly1305".to_string(),
        salt: STANDARD.encode(salt),
        nonce: STANDARD.encode(nonce),
        payload: STANDARD.encode(ciphertext),
    })
    .context("serialize encrypted config envelope")
}

fn decrypt_config(raw: &[u8], password: &str) -> Result<ConfigFile> {
    let envelope: EncryptedConfigEnvelope =
        serde_json::from_slice(raw).context("parse encrypted config envelope")?;
    if envelope.format_version != 1
        || envelope.kdf != "argon2id"
        || envelope.cipher != "xchacha20poly1305"
    {
        return Err(anyhow::anyhow!("unsupported encrypted config format"));
    }
    let salt = STANDARD
        .decode(envelope.salt)
        .context("decode config salt")?;
    let nonce = STANDARD
        .decode(envelope.nonce)
        .context("decode config nonce")?;
    if nonce.len() != 24 {
        return Err(anyhow::anyhow!("invalid config nonce"));
    }
    let ciphertext = STANDARD
        .decode(envelope.payload)
        .context("decode encrypted config payload")?;

    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(password.as_bytes(), &salt, &mut key)
        .map_err(|err| anyhow::anyhow!("derive encryption key: {err}"))?;

    let plaintext = XChaCha20Poly1305::new((&key).into())
        .decrypt(XNonce::from_slice(&nonce), ciphertext.as_ref())
        .map_err(|_| {
            anyhow::anyhow!("cannot decrypt config; hardware UUID mismatch or corrupted data")
        })?;

    serde_json::from_slice(&plaintext).context("parse decrypted config")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_hardware_uuid() {
        let uuid = get_hardware_uuid();
        assert!(!uuid.is_empty());
    }

    #[test]
    fn test_config_encryption_roundtrip() {
        let config = ConfigFile::default();
        let password = "test-password-123";
        let encrypted = encrypt_config(&config, password).unwrap();

        // Ensure it doesn't contain plain text fields of default config
        let encrypted_str = String::from_utf8_lossy(&encrypted);
        assert!(!encrypted_str.contains("Maple Mono NF CN"));
        assert!(encrypted_str.contains("argon2id"));

        let decrypted = decrypt_config(&encrypted, password).unwrap();
        assert_eq!(decrypted.terminal_font_family, config.terminal_font_family);

        // Decrypt with wrong password should fail
        assert!(decrypt_config(&encrypted, "wrong-password").is_err());
    }

    #[test]
    fn test_remember_tabs_defaults_to_disabled_for_older_configs() {
        let config: ConfigFile = serde_json::from_str("{}").unwrap();

        assert!(!config.remember_tabs);
        assert!(config.saved_tabs.is_none());
        assert!(config.sftp_tree_panels.is_none());
        assert!(config.sftp_file_columns.is_none());
        assert!(!config.sftp_file_columns_customized);
    }

    #[test]
    fn command_history_keeps_only_the_latest_duplicate() {
        let mut store = ConfigStore::in_memory();

        assert!(store.add_command_history("session-1", "first".to_string()));
        assert!(store.add_command_history("session-1", "second".to_string()));
        assert!(store.add_command_history("session-1", "first".to_string()));
        assert!(!store.add_command_history("session-1", "first".to_string()));

        assert_eq!(
            store.cache.command_history.get("session-1"),
            Some(&vec!["second".to_string(), "first".to_string()])
        );
        assert_eq!(store.cache.command_history_revision, 3);
    }

    #[test]
    fn test_saved_tabs_roundtrip() {
        let config = ConfigFile {
            remember_tabs: true,
            saved_tabs: Some(SavedTabsState {
                groups: vec![SavedTabGroup {
                    id: "group-1".to_string(),
                    title: "~".to_string(),
                    pane_root: SavedPaneLayout::Single {
                        tab_id: "tab-1".to_string(),
                    },
                    tabs: vec![SavedTerminalTab::Local {
                        id: "tab-1".to_string(),
                        cwd: Some(PathBuf::from("/tmp")),
                        terminal_encoding: TextEncoding::Utf8,
                    }],
                }],
                active_group: Some("group-1".to_string()),
                active_tab: Some("tab-1".to_string()),
            }),
            ..Default::default()
        };

        let json = serde_json::to_string(&config).unwrap();
        let restored: ConfigFile = serde_json::from_str(&json).unwrap();

        assert!(restored.remember_tabs);
        let restored_tabs = restored.saved_tabs.unwrap();
        assert_eq!(restored_tabs.groups.len(), 1);
        assert_eq!(restored_tabs.active_tab.as_deref(), Some("tab-1"));
    }

    #[test]
    fn test_save_merged_preferences() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("sessions.json");
        let mut store = ConfigStore {
            path: path.clone(),
            cache: ConfigFile::default(),
            write_lock: Arc::new(Mutex::new(())),
        };

        let session = Session {
            id: "test-session-id".to_string(),
            name: "Test Session".to_string(),
            host: "1.2.3.4".to_string(),
            port: 22,
            user: "root".to_string(),
            auth: AuthMethod::Password,
            password: "pwd".to_string(),
            private_key_path: String::new(),
            private_key_inline: String::new(),
            passphrase: String::new(),
            last_used: None,
            proxy_type: String::new(),
            proxy_host: String::new(),
            proxy_port: None,
            proxy_user: String::new(),
            proxy_password: String::new(),
            protocol: "ssh".to_string(),
            baud_rate: 115200,
            terminal_encoding: TextEncoding::Utf8,
        };
        store.cache.sessions.push(session.clone());
        store.save().unwrap();

        let mut local_config = ConfigFile {
            ui_font_size: 18.0,
            terminal_font_size: 20.0,
            show_hidden_files: true,
            sftp_file_columns_customized: true,
            remember_tabs: true,
            use_proxy: true,
            read_env_proxy: false,
            global_proxy_type: "http".to_string(),
            global_proxy_host: "proxy.example.com".to_string(),
            global_proxy_port: Some(8080),
            global_proxy_user: "proxy-user".to_string(),
            global_proxy_password: "proxy-password".to_string(),
            command_history_revision: 2,
            saved_tabs: Some(SavedTabsState {
                groups: Vec::new(),
                active_group: None,
                active_tab: None,
            }),
            ..Default::default()
        };
        local_config
            .key_bindings
            .insert("QuitApplication".to_string(), "cmd-q".to_string());
        local_config.command_history.insert(
            "test-session-id".to_string(),
            vec!["pwd".to_string(), "ls -la".to_string()],
        );

        let mut stale_config = local_config.clone();
        stale_config.command_history.clear();
        stale_config.command_history_revision = 1;
        store.save_merged_preferences(local_config).unwrap();
        store.save_merged_preferences(stale_config).unwrap();

        let loaded_bytes = fs::read(&path).unwrap();
        let decrypted = decrypt_config(&loaded_bytes, &get_hardware_uuid()).unwrap();

        assert_eq!(decrypted.ui_font_size, 18.0);
        assert_eq!(decrypted.terminal_font_size, 20.0);
        assert!(decrypted.show_hidden_files);
        assert!(decrypted.sftp_file_columns_customized);
        assert!(decrypted.remember_tabs);
        assert!(decrypted.saved_tabs.is_some());
        assert_eq!(
            decrypted
                .key_bindings
                .get("QuitApplication")
                .map(String::as_str),
            Some("cmd-q")
        );
        assert!(decrypted.use_proxy);
        assert!(!decrypted.read_env_proxy);
        assert_eq!(decrypted.global_proxy_type, "http");
        assert_eq!(decrypted.global_proxy_host, "proxy.example.com");
        assert_eq!(decrypted.global_proxy_port, Some(8080));
        assert_eq!(decrypted.global_proxy_user, "proxy-user");
        assert_eq!(decrypted.global_proxy_password, "proxy-password");
        assert_eq!(
            decrypted.command_history.get("test-session-id"),
            Some(&vec!["pwd".to_string(), "ls -la".to_string()])
        );
        assert_eq!(decrypted.command_history_revision, 2);

        assert_eq!(decrypted.sessions.len(), 1);
        assert_eq!(decrypted.sessions[0].name, "Test Session");
        assert_eq!(decrypted.sessions[0].host, "1.2.3.4");
    }

    #[test]
    fn test_key_binding_unbinding_none() {
        let mut store = ConfigStore::in_memory();
        store.set_key_binding("OpenSettings", "none");
        assert_eq!(store.key_bindings().get("OpenSettings").unwrap(), "none");
    }
}

use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use anyhow::{Context as _, Result, anyhow, bail};
use csv::{ReaderBuilder, StringRecord, Terminator, WriterBuilder};
use gpui::{Context, Entity, SharedString};
use gpui_component::{WindowExt as _, dialog::DialogButtonProps, input::InputState};
use rust_i18n::t;
use serde::{Deserialize, Serialize};

use crate::{
    Ashell,
    app::DialogKind,
    session::{
        config::{AuthMethod, Session},
        ssh_keys::normalize_inline_private_key,
    },
    sync::{self, SyncBackendCredentials, SyncCredentials, SyncPayload, SyncResult},
    terminal::BackendEvent,
};

const CONNECTION_CSV_HEADERS: [&str; 6] = ["name", "group", "host", "port", "username", "password"];

#[derive(Debug, Serialize)]
struct ConnectionCsvExportRow<'a> {
    name: &'a str,
    group: &'a str,
    host: &'a str,
    port: u16,
    username: &'a str,
    password: &'a str,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
struct ConnectionCsvRow {
    #[serde(default)]
    name: String,
    #[serde(default)]
    group: String,
    #[serde(default)]
    host: String,
    #[serde(default)]
    port: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    auth_type: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    private_key_path: String,
    #[serde(default)]
    private_key_content: String,
    #[serde(default)]
    passphrase: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ConnectionCsvColumns {
    name: bool,
    group: bool,
    auth_type: bool,
    password: bool,
    private_key_path: bool,
    private_key_content: bool,
    passphrase: bool,
}

#[derive(Debug)]
struct ImportedCsvConnection {
    session: Session,
    columns: ConnectionCsvColumns,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ConnectionIdentity {
    host: String,
    port: u16,
    username: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ConnectionImportSummary {
    added: usize,
    updated: usize,
    skipped: usize,
}

fn normalize_csv_header(header: &str) -> String {
    header
        .trim_start_matches('\u{feff}')
        .trim()
        .to_lowercase()
        .chars()
        .map(|character| match character {
            ' ' | '-' => '_',
            _ => character,
        })
        .collect()
}

fn canonical_csv_header(header: &str) -> Option<&'static str> {
    match normalize_csv_header(header).as_str() {
        "name" | "title" | "session" | "session_name" | "connection_name" => Some("name"),
        "group" | "folder" | "category" | "connection_group" => Some("group"),
        "host" | "hostname" | "host_name" | "ip" | "ip_address" | "address" | "server" => {
            Some("host")
        }
        "port" | "ssh_port" => Some("port"),
        "username" | "user" | "user_name" | "login" | "login_name" => Some("username"),
        "auth_type"
        | "auth"
        | "authentication"
        | "authentication_type"
        | "auth_method"
        | "method" => Some("auth_type"),
        "password" | "pass" | "passwd" => Some("password"),
        "private_key_path" | "key_path" | "identity_file" | "identityfile" | "privatekeypath" => {
            Some("private_key_path")
        }
        "private_key_content"
        | "key_content"
        | "private_key"
        | "privatekey"
        | "private_key_data" => Some("private_key_content"),
        "passphrase" | "key_password" | "key_passphrase" => Some("passphrase"),
        _ => None,
    }
}

fn parse_auth_method(value: &str, inferred: AuthMethod, line: u64) -> Result<AuthMethod> {
    let normalized = normalize_csv_header(value);
    match normalized.as_str() {
        "" => Ok(inferred),
        "password" | "pass" => Ok(AuthMethod::Password),
        "key" | "private_key" | "public_key" => Ok(AuthMethod::Key),
        "config" | "ssh_config" => Ok(AuthMethod::Config),
        _ => bail!("CSV line {line}: unsupported auth_type '{value}'"),
    }
}

impl ConnectionCsvColumns {
    fn from_headers(headers: &HashSet<&'static str>) -> Self {
        Self {
            name: headers.contains("name"),
            group: headers.contains("group"),
            auth_type: headers.contains("auth_type"),
            password: headers.contains("password"),
            private_key_path: headers.contains("private_key_path"),
            private_key_content: headers.contains("private_key_content"),
            passphrase: headers.contains("passphrase"),
        }
    }
}

impl ConnectionCsvRow {
    fn into_connection(
        self,
        line: u64,
        columns: ConnectionCsvColumns,
    ) -> Result<ImportedCsvConnection> {
        let Self {
            name,
            group,
            host,
            port,
            username,
            auth_type,
            password,
            private_key_path,
            private_key_content,
            passphrase,
        } = self;
        let host = host.trim().to_string();
        let username = username.trim().to_string();
        if host.is_empty() {
            bail!("CSV line {line}: host is required");
        }
        if username.is_empty() {
            bail!("CSV line {line}: username is required");
        }

        let port = if port.trim().is_empty() {
            22
        } else {
            port.trim()
                .parse::<u16>()
                .with_context(|| format!("CSV line {line}: invalid port '{}'", port.trim()))?
        };
        if port == 0 {
            bail!("CSV line {line}: port must be between 1 and 65535");
        }

        let private_key_path = private_key_path.trim().to_string();
        let private_key_content = normalize_inline_private_key(&private_key_content);
        let inferred_auth = if !private_key_path.is_empty()
            || !private_key_content.is_empty()
            || password.is_empty()
        {
            AuthMethod::Key
        } else {
            AuthMethod::Password
        };
        let auth = parse_auth_method(&auth_type, inferred_auth, line)?;

        let mut session = Session::password(host, port, username, password);
        session.group = group.trim().to_string();
        session.auth = auth;
        session.private_key_path = if private_key_content.is_empty() {
            private_key_path
        } else {
            String::new()
        };
        session.private_key_inline = private_key_content;
        session.passphrase = passphrase;
        if !name.trim().is_empty() {
            session.name = name.trim().to_string();
        }
        Ok(ImportedCsvConnection { session, columns })
    }
}

fn decode_connection_csv(bytes: &[u8]) -> Result<Vec<ImportedCsvConnection>> {
    std::str::from_utf8(bytes).context("connection CSV must be UTF-8")?;
    let mut reader = ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .trim(csv::Trim::Headers)
        .from_reader(bytes);
    let source_headers = reader
        .headers()
        .context("failed to read CSV header")?
        .clone();
    if source_headers.is_empty() {
        bail!("connection CSV has no header");
    }

    let mut recognized_headers = HashSet::new();
    let normalized_headers = source_headers
        .iter()
        .enumerate()
        .map(|(index, header)| {
            if let Some(canonical) = canonical_csv_header(header) {
                if !recognized_headers.insert(canonical) {
                    bail!("connection CSV contains duplicate '{canonical}' columns");
                }
                Ok(canonical.to_string())
            } else {
                Ok(format!("__ignored_{index}"))
            }
        })
        .collect::<Result<Vec<_>>>()?;
    for required in ["host", "username"] {
        if !recognized_headers.contains(required) {
            bail!("connection CSV is missing the '{required}' column");
        }
    }
    let columns = ConnectionCsvColumns::from_headers(&recognized_headers);

    let normalized_headers = StringRecord::from(normalized_headers);
    reader.set_headers(normalized_headers.clone());
    let mut connections = Vec::new();
    for (index, record) in reader.records().enumerate() {
        let record = record.map_err(|err| {
            let line = err
                .position()
                .map(|position| position.line())
                .unwrap_or(index as u64 + 2);
            anyhow!("CSV line {line}: {err}")
        })?;
        let line = index as u64 + 2;
        let row = record
            .deserialize::<ConnectionCsvRow>(Some(&normalized_headers))
            .map_err(|err| anyhow!("CSV line {line}: {err}"))?;
        connections.push(row.into_connection(line, columns)?);
    }
    Ok(connections)
}

fn encode_connection_csv(sessions: &[Session]) -> Result<Vec<u8>> {
    let mut output = vec![0xef, 0xbb, 0xbf];
    {
        let mut writer = WriterBuilder::new()
            .has_headers(false)
            .terminator(Terminator::CRLF)
            .from_writer(&mut output);
        writer
            .write_record(CONNECTION_CSV_HEADERS)
            .context("failed to write CSV header")?;
        for session in sessions {
            let row = ConnectionCsvExportRow {
                name: &session.name,
                group: &session.group,
                host: &session.host,
                port: session.port,
                username: &session.user,
                password: if session.auth == AuthMethod::Password {
                    &session.password
                } else {
                    ""
                },
            };
            writer
                .serialize(row)
                .context("failed to serialize connection CSV")?;
        }
        writer.flush().context("failed to finish connection CSV")?;
    }
    Ok(output)
}

fn write_connection_csv(path: &Path, contents: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::{
            io::Write as _,
            os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
        };

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        file.write_all(contents)
            .with_context(|| format!("failed to write {}", path.display()))?;
        let mut permissions = file
            .metadata()
            .with_context(|| format!("failed to inspect {}", path.display()))?
            .permissions();
        permissions.set_mode(0o600);
        file.set_permissions(permissions)
            .with_context(|| format!("failed to protect {}", path.display()))?;
    }

    #[cfg(not(unix))]
    std::fs::write(path, contents)
        .with_context(|| format!("failed to write {}", path.display()))?;

    Ok(())
}

fn is_ssh_session(session: &Session) -> bool {
    session.protocol.eq_ignore_ascii_case("ssh")
}

fn connection_identity(session: &Session) -> ConnectionIdentity {
    ConnectionIdentity {
        host: session.host.trim().to_lowercase(),
        port: session.port,
        username: session.user.trim().to_string(),
    }
}

fn deduplicate_csv_connections(
    connections: Vec<ImportedCsvConnection>,
) -> Vec<ImportedCsvConnection> {
    let mut positions = HashMap::<ConnectionIdentity, usize>::new();
    let mut deduplicated = Vec::new();
    for connection in connections {
        let identity = connection_identity(&connection.session);
        if let Some(index) = positions.get(&identity).copied() {
            deduplicated[index] = connection;
        } else {
            positions.insert(identity, deduplicated.len());
            deduplicated.push(connection);
        }
    }
    deduplicated
}

fn update_csv_session(existing: &mut Session, imported: ImportedCsvConnection) {
    let ImportedCsvConnection { session, columns } = imported;
    if columns.name {
        existing.name = session.name;
    }
    if columns.group {
        existing.group = session.group;
    }
    existing.host = session.host;
    existing.port = session.port;
    existing.user = session.user;

    if columns.password
        || columns.auth_type
        || columns.private_key_path
        || columns.private_key_content
    {
        existing.auth = session.auth;
    }
    if columns.password {
        existing.password = session.password;
    }
    if columns.private_key_content {
        existing.private_key_inline = session.private_key_inline.clone();
        if !session.private_key_inline.is_empty() {
            existing.private_key_path.clear();
        }
    }
    if columns.private_key_path && session.private_key_inline.is_empty() {
        existing.private_key_path = session.private_key_path;
        if !existing.private_key_path.is_empty() {
            existing.private_key_inline.clear();
        }
    }
    if columns.passphrase {
        existing.passphrase = session.passphrase;
    }
}

fn merge_csv_sessions(
    local_sessions: &mut Vec<Session>,
    imported_connections: Vec<ImportedCsvConnection>,
) -> ConnectionImportSummary {
    let mut summary = ConnectionImportSummary::default();
    for imported in deduplicate_csv_connections(imported_connections) {
        let identity = connection_identity(&imported.session);
        let mut matches = local_sessions
            .iter()
            .enumerate()
            .filter(|(_, local)| is_ssh_session(local) && connection_identity(local) == identity)
            .map(|(index, _)| index);
        let first_match = matches.next();
        if matches.next().is_some() {
            summary.skipped += 1;
            continue;
        }
        if let Some(index) = first_match {
            update_csv_session(&mut local_sessions[index], imported);
            summary.updated += 1;
            continue;
        }

        local_sessions.push(imported.session);
        summary.added += 1;
    }
    summary
}

impl Ashell {
    fn sync_input_value(input: &Entity<InputState>, cx: &Context<Self>) -> String {
        input.read(cx).value().trim().to_string()
    }

    fn sync_credentials(&self, cx: &Context<Self>) -> SyncCredentials {
        let backend = if self.config.sync_backend() == "s3" {
            SyncBackendCredentials::S3 {
                endpoint: Self::sync_input_value(&self.sync_s3_endpoint_input, cx),
                region: Self::sync_input_value(&self.sync_s3_region_input, cx),
                bucket: Self::sync_input_value(&self.sync_s3_bucket_input, cx),
                object_key: Self::sync_input_value(&self.sync_s3_object_key_input, cx),
                access_key: Self::sync_input_value(&self.sync_s3_access_key_input, cx),
                secret_key: self.sync_s3_secret_key_input.read(cx).value().to_string(),
                session_token: self
                    .sync_s3_session_token_input
                    .read(cx)
                    .value()
                    .to_string(),
            }
        } else {
            SyncBackendCredentials::WebDav {
                endpoint: Self::sync_input_value(&self.sync_endpoint_input, cx),
                username: Self::sync_input_value(&self.sync_username_input, cx),
                password: self.sync_webdav_password_input.read(cx).value().to_string(),
            }
        };
        SyncCredentials {
            backend,
            encryption_password: self
                .sync_encryption_password_input
                .read(cx)
                .value()
                .to_string(),
        }
    }

    fn begin_sync(
        &mut self,
        status: SharedString,
        cx: &mut Context<Self>,
    ) -> Option<SyncCredentials> {
        if self.sync_in_progress {
            return None;
        }
        let credentials = self.sync_credentials(cx);
        match &credentials.backend {
            SyncBackendCredentials::WebDav {
                endpoint, username, ..
            } => {
                self.config
                    .set_sync_connection(endpoint.clone(), username.clone());
            }
            SyncBackendCredentials::S3 {
                endpoint,
                region,
                bucket,
                object_key,
                ..
            } => {
                self.config.set_sync_s3_connection(
                    endpoint.clone(),
                    region.clone(),
                    bucket.clone(),
                    object_key.clone(),
                );
            }
        }
        if let Err(err) = self.config.save() {
            self.sync_status = format!("{}: {err:#}", t!("sync_failed")).into();
            cx.notify();
            return None;
        }
        self.sync_in_progress = true;
        self.sync_status = status;
        cx.notify();
        Some(credentials)
    }

    pub(crate) fn set_sync_backend(&mut self, backend: &str, cx: &mut Context<Self>) {
        self.config.set_sync_backend(backend);
        let _ = self.config.save();
        self.sync_status = t!("sync_not_run").into();
        cx.notify();
    }

    pub(crate) fn upload_sync_config(&mut self, cx: &mut Context<Self>) {
        let Some(credentials) = self.begin_sync(t!("sync_uploading").into(), cx) else {
            return;
        };
        let payload = SyncPayload::new(
            self.config.sync_device_id().to_string(),
            self.config.sessions().to_vec(),
        );
        let expected_etag = self.config.sync_etag().map(str::to_string);
        let events = self.events_tx.clone();
        self.runtime.spawn(async move {
            let result = match sync::upload(credentials, payload, expected_etag).await {
                Ok(etag) => SyncResult::Uploaded { etag },
                Err(err) => SyncResult::Failed(format!("{err:#}")),
            };
            let _ = events.send(BackendEvent::SyncFinished(result));
        });
    }

    pub(crate) fn download_sync_config(&mut self, cx: &mut Context<Self>) {
        let Some(credentials) = self.begin_sync(t!("sync_downloading").into(), cx) else {
            return;
        };
        let events = self.events_tx.clone();
        self.runtime.spawn(async move {
            let result = match sync::download(credentials).await {
                Ok((payload, etag)) => SyncResult::Downloaded { payload, etag },
                Err(err) => SyncResult::Failed(format!("{err:#}")),
            };
            let _ = events.send(BackendEvent::SyncFinished(result));
        });
    }

    pub(crate) fn export_local_config(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let local_config = self.config.cache.clone();
        let file_dialog = rfd::AsyncFileDialog::new()
            .set_file_name("ashell-config.json")
            .add_filter("JSON", &["json"])
            .save_file();

        cx.spawn_in(window, async move |_this, cx| {
            if let Some(file_handle) = file_dialog.await {
                let path = file_handle.path().to_path_buf();
                if let Ok(json_str) = serde_json::to_string_pretty(&local_config) {
                    let _ = cx
                        .background_executor()
                        .spawn(async move {
                            if let Err(err) = std::fs::write(path, json_str) {
                                tracing::error!("failed to export local config: {err:#}");
                            }
                        })
                        .await;
                }
            }
            Ok::<(), anyhow::Error>(())
        })
        .detach();
    }

    pub(crate) fn import_local_config(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let file_dialog = rfd::AsyncFileDialog::new()
            .add_filter("JSON", &["json"])
            .pick_file();

        cx.spawn_in(window, async move |this, cx| {
            if let Some(file_handle) = file_dialog.await {
                let path = file_handle.path().to_path_buf();
                let read_result = cx
                    .background_executor()
                    .spawn(async move { std::fs::read_to_string(path) })
                    .await;

                if let Ok(json_str) = read_result {
                    if let Ok(config_file) =
                        serde_json::from_str::<crate::session::config::ConfigFile>(&json_str)
                    {
                        let _ = gpui::AsyncWindowContext::update(cx, |window, cx| {
                            let _ = this.update(cx, |this, cx| {
                                this.config.cache = config_file;
                                this.config.normalize_command_history();
                                this.selected_connection_ids.clear();
                                if let Err(err) = this.config.save() {
                                    tracing::error!("failed to save imported config: {err:#}");
                                } else {
                                    this.apply_loaded_config(window, cx);
                                }
                            });
                        });
                    }
                }
            }
            Ok::<(), anyhow::Error>(())
        })
        .detach();
    }

    pub(crate) fn export_connections(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if self.active_dialog.is_some() {
            return;
        }

        let export_selected = !self.selected_connection_ids.is_empty();
        let candidates = self
            .config
            .sessions()
            .iter()
            .filter(|session| {
                !export_selected || self.selected_connection_ids.contains(&session.id)
            })
            .cloned()
            .collect::<Vec<_>>();
        let skipped = candidates
            .iter()
            .filter(|session| !is_ssh_session(session))
            .count();
        let sessions = candidates
            .into_iter()
            .filter(is_ssh_session)
            .collect::<Vec<_>>();
        if sessions.is_empty() {
            self.status = t!("connections_export_no_ssh").to_string().into();
            cx.notify();
            return;
        }

        let count = sessions.len();
        let title = t!("connections_export_plaintext_title").to_string();
        let description = if skipped == 0 {
            t!("connections_export_plaintext_warning", count = count).to_string()
        } else {
            t!(
                "connections_export_plaintext_warning_with_skipped",
                count = count,
                skipped = skipped
            )
            .to_string()
        };
        let confirm_text = t!("export_connections").to_string();
        let cancel_text = t!("cancel").to_string();
        let view = cx.entity();
        let view_for_cancel = view.clone();
        self.active_dialog = Some(DialogKind::ConnectionExport);
        cx.notify();

        window.open_alert_dialog(cx, move |alert, _, _| {
            let view_for_ok = view.clone();
            let sessions_for_export = sessions.clone();
            alert
                .title(title.clone())
                .description(description.clone())
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(confirm_text.clone())
                        .cancel_text(cancel_text.clone())
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    let sessions = sessions_for_export.clone();
                    view_for_ok.update(cx, |this, cx| {
                        if this.active_dialog == Some(DialogKind::ConnectionExport) {
                            this.active_dialog = None;
                        }
                        this.start_connection_csv_export(sessions, window, cx);
                        cx.notify();
                    });
                    true
                })
                .on_cancel({
                    let view = view_for_cancel.clone();
                    move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            if this.active_dialog == Some(DialogKind::ConnectionExport) {
                                this.active_dialog = None;
                            }
                            cx.notify();
                        });
                        true
                    }
                })
        });
    }

    fn start_connection_csv_export(
        &mut self,
        sessions: Vec<Session>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let count = sessions.len();
        cx.spawn_in(window, async move |this, cx| {
            let encoded = cx
                .background_executor()
                .spawn(async move { encode_connection_csv(&sessions) })
                .await;
            let contents = match encoded {
                Ok(contents) => contents,
                Err(err) => {
                    tracing::error!("failed to prepare connection export: {err:#}");
                    let reason = err.to_string();
                    let _ = gpui::AsyncWindowContext::update(cx, |_, cx| {
                        let _ = this.update(cx, |this, cx| {
                            this.status = t!("connections_export_failed", reason = reason)
                                .to_string()
                                .into();
                            cx.notify();
                        });
                    });
                    return Ok::<(), anyhow::Error>(());
                }
            };

            let file_dialog = rfd::AsyncFileDialog::new()
                .set_file_name("ashell-connections.csv")
                .add_filter("CSV", &["csv"])
                .save_file();
            if let Some(file_handle) = file_dialog.await {
                let path = file_handle.path().to_path_buf();
                let result = cx
                    .background_executor()
                    .spawn(async move { write_connection_csv(&path, &contents) })
                    .await;
                let _ = gpui::AsyncWindowContext::update(cx, |_, cx| {
                    let _ = this.update(cx, |this, cx| {
                        this.status = match result {
                            Ok(()) => t!("connections_exported", count = count).to_string().into(),
                            Err(err) => {
                                tracing::error!("failed to export connections: {err:#}");
                                t!("connections_export_failed", reason = err.to_string())
                                    .to_string()
                                    .into()
                            }
                        };
                        cx.notify();
                    });
                });
            }
            Ok::<(), anyhow::Error>(())
        })
        .detach();
    }

    pub(crate) fn import_connections(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let file_dialog = rfd::AsyncFileDialog::new()
            .add_filter("CSV", &["csv"])
            .pick_file();

        cx.spawn_in(window, async move |this, cx| {
            if let Some(file_handle) = file_dialog.await {
                let path = file_handle.path().to_path_buf();
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        let contents = std::fs::read(&path)
                            .with_context(|| format!("failed to read {}", path.display()))?;
                        decode_connection_csv(&contents)
                    })
                    .await;

                let _ = gpui::AsyncWindowContext::update(cx, |_, cx| {
                    let _ = this.update(cx, |this, cx| {
                        match result {
                            Ok(sessions) => {
                                let previous_config = this.config.cache.clone();
                                let summary =
                                    merge_csv_sessions(&mut this.config.cache.sessions, sessions);
                                this.config.sync_connection_groups_from_sessions();

                                match this.config.save() {
                                    Ok(()) => {
                                        this.selected_connection_ids.clear();
                                        this.status = t!(
                                            "connections_import_summary",
                                            added = summary.added,
                                            updated = summary.updated,
                                            skipped = summary.skipped
                                        )
                                        .to_string()
                                        .into();
                                    }
                                    Err(err) => {
                                        this.config.cache = previous_config;
                                        tracing::error!(
                                            "failed to save imported connections: {err:#}"
                                        );
                                        this.status = t!(
                                            "connections_import_failed",
                                            reason = err.to_string()
                                        )
                                        .to_string()
                                        .into();
                                    }
                                }
                            }
                            Err(err) => {
                                tracing::error!("failed to import connections: {err:#}");
                                this.status =
                                    t!("connections_import_failed", reason = err.to_string())
                                        .to_string()
                                        .into();
                            }
                        }
                        cx.notify();
                    });
                });
            }
            Ok::<(), anyhow::Error>(())
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn password_session(id: &str, name: &str, host: &str, user: &str) -> Session {
        let mut session =
            Session::password(host.to_string(), 22, user.to_string(), "secret".to_string());
        session.id = id.to_string();
        session.name = name.to_string();
        session
    }

    #[test]
    fn csv_export_uses_minimal_columns_and_preserves_quoted_passwords() {
        let mut password = password_session("password-id", "Primary, server", "one.test", "root");
        password.password = "line one,\nline two".to_string();
        password.group = "Production".to_string();

        let mut key = Session::key(
            "two.test".to_string(),
            2202,
            "deploy".to_string(),
            "/keys/id_ed25519".to_string(),
            "-----BEGIN PRIVATE KEY-----\nkey data\n-----END PRIVATE KEY-----\n".to_string(),
            "key phrase".to_string(),
        );
        key.id = "key-id".to_string();
        key.name = "Key server".to_string();

        let bytes = encode_connection_csv(&[password, key]).expect("encode CSV");
        assert!(bytes.starts_with(&[0xef, 0xbb, 0xbf]));
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("\r\n"));
        assert_eq!(
            text.trim_start_matches('\u{feff}').split("\r\n").next(),
            Some("name,group,host,port,username,password")
        );
        assert!(!text.contains("auth_type"));
        assert!(!text.contains("private_key_path"));
        assert!(!text.contains("private_key_content"));
        assert!(!text.contains("passphrase"));
        assert!(!text.contains("password-id"));
        assert!(!text.contains("BEGIN PRIVATE KEY"));
        assert!(!text.contains("key phrase"));

        let connections = decode_connection_csv(&bytes).expect("decode CSV");
        assert_eq!(connections.len(), 2);
        assert_eq!(connections[0].session.group, "Production");
        assert_eq!(connections[0].session.password, "line one,\nline two");
        assert_eq!(connections[0].session.auth, AuthMethod::Password);
        assert_eq!(connections[1].session.auth, AuthMethod::Key);
        assert!(connections[1].session.password.is_empty());
        assert!(connections[1].session.private_key_path.is_empty());
        assert!(connections[1].session.private_key_inline.is_empty());
        assert!(connections[1].session.passphrase.is_empty());
    }

    #[test]
    fn csv_import_accepts_common_aliases_and_defaults_the_port() {
        let csv = concat!(
            "Title,Hostname,SSH Port,User,Authentication,Pass,Ignored Column\r\n",
            "Production,EXAMPLE.test,,root,password,secret,ignored\r\n"
        );
        let connections = decode_connection_csv(csv.as_bytes()).expect("decode aliased CSV");
        assert_eq!(connections.len(), 1);
        assert_eq!(connections[0].session.name, "Production");
        assert_eq!(connections[0].session.host, "EXAMPLE.test");
        assert_eq!(connections[0].session.port, 22);
        assert_eq!(connections[0].session.user, "root");
        assert_eq!(connections[0].session.password, "secret");
        assert!(connections[0].columns.auth_type);
    }

    #[test]
    fn csv_import_reports_the_invalid_row() {
        let csv = "host,username,port\r\none.test,root,22\r\ntwo.test,root,70000\r\n";
        let error = decode_connection_csv(csv.as_bytes()).expect_err("invalid port must fail");
        assert!(
            error.to_string().contains("CSV line 3"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn csv_import_remains_compatible_with_optional_legacy_columns() {
        let csv = concat!(
            "name,host,username,auth_type,private_key_path,private_key_content,passphrase,id\r\n",
            "Legacy,key.test,root,key,/keys/id_test,",
            "\"-----BEGIN PRIVATE KEY-----\\nkey\\n-----END PRIVATE KEY-----\",",
            "key phrase,external-id\r\n"
        );
        let connections = decode_connection_csv(csv.as_bytes()).expect("decode legacy CSV");
        assert_eq!(connections.len(), 1);
        let connection = &connections[0];
        assert_eq!(connection.session.auth, AuthMethod::Key);
        assert!(connection.session.private_key_path.is_empty());
        assert!(
            connection
                .session
                .private_key_inline
                .contains("BEGIN PRIVATE KEY")
        );
        assert_eq!(connection.session.passphrase, "key phrase");
        assert_ne!(connection.session.id, "external-id");
        assert!(connection.columns.private_key_path);
        assert!(connection.columns.private_key_content);
        assert!(connection.columns.passphrase);
    }

    #[test]
    fn csv_merge_uses_the_last_duplicate_row() {
        let csv = concat!(
            "name,host,port,username,password\r\n",
            "First,same.test,22,root,old\r\n",
            "Last,SAME.test,22,root,latest\r\n"
        );
        let imported = decode_connection_csv(csv.as_bytes()).expect("decode duplicate CSV");
        let mut local = Vec::new();

        let summary = merge_csv_sessions(&mut local, imported);
        assert_eq!(
            summary,
            ConnectionImportSummary {
                added: 1,
                updated: 0,
                skipped: 0,
            }
        );
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].name, "Last");
        assert_eq!(local[0].password, "latest");
    }

    #[test]
    fn csv_merge_preserves_local_only_fields_and_skips_ambiguous_matches() {
        let mut identity_match = Session::key(
            "match.test".to_string(),
            22,
            "deploy".to_string(),
            "/keys/local".to_string(),
            String::new(),
            "local phrase".to_string(),
        );
        identity_match.id = "local-id".to_string();
        identity_match.name = "Identity".to_string();
        identity_match.group = "Existing group".to_string();
        identity_match.proxy_type = "socks5".to_string();
        identity_match.proxy_host = "proxy.test".to_string();
        identity_match.last_used = Some("2026-08-14T10:00:00Z".to_string());
        let duplicate_one = password_session("duplicate-1", "Duplicate 1", "dupe.test", "root");
        let duplicate_two = password_session("duplicate-2", "Duplicate 2", "DUPE.test", "root");
        let mut local = vec![identity_match, duplicate_one, duplicate_two];
        let csv = concat!(
            "name,host,port,username,password\r\n",
            "Matched,MATCH.test,22,deploy,\r\n",
            "Added,added.test,22,root,new password\r\n",
            "Ambiguous,dupe.test,22,root,password\r\n"
        );
        let imported = decode_connection_csv(csv.as_bytes()).expect("decode merge CSV");

        let summary = merge_csv_sessions(&mut local, imported);
        assert_eq!(
            summary,
            ConnectionImportSummary {
                added: 1,
                updated: 1,
                skipped: 1,
            }
        );

        let identity = local
            .iter()
            .find(|session| session.id == "local-id")
            .expect("identity match");
        assert_eq!(identity.name, "Matched");
        assert_eq!(identity.group, "Existing group");
        assert_eq!(identity.auth, AuthMethod::Key);
        assert_eq!(identity.private_key_path, "/keys/local");
        assert_eq!(identity.passphrase, "local phrase");
        assert_eq!(identity.proxy_type, "socks5");
        assert_eq!(identity.proxy_host, "proxy.test");
        assert_eq!(identity.last_used.as_deref(), Some("2026-08-14T10:00:00Z"));
        assert!(local.iter().any(|session| session.host == "added.test"));
    }

    #[test]
    fn csv_merge_ignores_legacy_ids_and_preserves_an_unlisted_name() {
        let local = password_session("local-id", "Custom name", "same.test", "root");
        let csv = concat!(
            "host,port,username,password,id\r\n",
            "same.test,22,root,new password,other-id\r\n"
        );
        let imported = decode_connection_csv(csv.as_bytes()).expect("decode CSV with legacy ID");
        let mut sessions = vec![local];

        let summary = merge_csv_sessions(&mut sessions, imported);
        assert_eq!(summary.updated, 1);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "local-id");
        assert_eq!(sessions[0].name, "Custom name");
        assert_eq!(sessions[0].password, "new password");
    }

    #[test]
    fn csv_import_updates_group_only_when_the_column_is_present() {
        let mut local = password_session("local-id", "Local", "same.test", "root");
        local.group = "Old group".to_string();
        let mut sessions = vec![local];

        let without_group = decode_connection_csv(
            b"name,host,port,username,password\r\nUpdated,same.test,22,root,new\r\n",
        )
        .expect("decode CSV without group");
        merge_csv_sessions(&mut sessions, without_group);
        assert_eq!(sessions[0].group, "Old group");

        let with_group = decode_connection_csv(
            b"name,group,host,port,username,password\r\nUpdated,New group,same.test,22,root,new\r\n",
        )
        .expect("decode CSV with group");
        merge_csv_sessions(&mut sessions, with_group);
        assert_eq!(sessions[0].group, "New group");
    }
}

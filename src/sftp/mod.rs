pub mod ops;

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use directories::BaseDirs;
use flate2::read::GzDecoder;
use russh::{
    Disconnect,
    client::{self, Handler},
    keys::{PrivateKey, decode_secret_key, load_secret_key},
};
use russh_sftp::{client::SftpSession, protocol::FileAttributes};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{
        mpsc::{self, UnboundedReceiver, UnboundedSender},
        oneshot,
    },
    task::JoinHandle,
};
use uuid::Uuid;
use walkdir::WalkDir;
use zip::read::ZipArchive;

use rust_i18n::t;

use crate::{
    session::{
        config::{AuthMethod, Session},
        ssh_keys::{
            authenticate_with_default_keys, normalize_inline_private_key, private_keys_with_algs,
            session_has_explicit_key,
        },
    },
    terminal::BackendEvent,
};

#[derive(Debug, Clone)]
pub struct RemoteEntry {
    pub name: String,
    pub full_path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: u32,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct PreviewData {
    pub path: String,
    pub title: String,
    pub body: String,
    pub is_binary: bool,
}

#[derive(Debug)]
pub enum SftpCommand {
    ListDir(String),
    #[allow(dead_code)]
    Preview(String),
    Download {
        remote: String,
        local_dir: String,
    },
    ReadTextFile {
        remote_path: String,
        reply: oneshot::Sender<std::result::Result<Vec<u8>, String>>,
    },
    WriteTextFile {
        remote_path: String,
        content: Vec<u8>,
        reply: oneshot::Sender<std::result::Result<(), String>>,
    },
    RenamePath {
        old_path: String,
        new_path: String,
        reply: oneshot::Sender<std::result::Result<(), String>>,
    },
    CreateDir(String),
    DeletePaths(Vec<String>),
    UploadPaths {
        locals: Vec<String>,
        remote_dir: String,
    },
    PauseTransfer(String),
    ResumeTransfer(String),
    CancelTransfer(String),
    TransferFinished(String),
    Close,
}

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

pub struct TransferStateFlag(pub Arc<AtomicU8>);

#[derive(Clone, Copy)]
struct TransferContext<'a> {
    flag: &'a TransferStateFlag,
    events: &'a std::sync::mpsc::Sender<BackendEvent>,
    tab_id: &'a str,
    id: &'a str,
}

impl TransferStateFlag {
    pub fn new() -> Self {
        Self(Arc::new(AtomicU8::new(0)))
    }

    pub fn pause(&self) {
        self.0.store(1, Ordering::SeqCst);
    }
    pub fn resume(&self) {
        self.0.store(0, Ordering::SeqCst);
    }
    pub fn cancel(&self) {
        self.0.store(2, Ordering::SeqCst);
    }

    pub async fn yield_if_paused(
        &self,
        events: &std::sync::mpsc::Sender<crate::terminal::BackendEvent>,
        tab_id: &str,
        id: &str,
        transferred: u64,
        total: Option<u64>,
    ) -> anyhow::Result<()> {
        let mut was_paused = false;
        loop {
            let state = self.0.load(Ordering::SeqCst);
            if state == 2 {
                return Err(anyhow::anyhow!("transfer cancelled"));
            }
            if state == 1 {
                if !was_paused {
                    let _ = events.send(crate::terminal::BackendEvent::TransferProgress {
                        tab_id: tab_id.to_string(),
                        id: id.to_string(),
                        transferred,
                        total,
                        state: crate::terminal::TransferState::Paused,
                    });
                    was_paused = true;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            } else {
                if was_paused {
                    let _ = events.send(crate::terminal::BackendEvent::TransferProgress {
                        tab_id: tab_id.to_string(),
                        id: id.to_string(),
                        transferred,
                        total,
                        state: crate::terminal::TransferState::Running,
                    });
                }
                return Ok(());
            }
        }
    }
}

pub struct SftpHandle {
    pub commands: UnboundedSender<SftpCommand>,
    #[allow(dead_code)]
    join: Option<JoinHandle<()>>,
}

impl Clone for SftpHandle {
    fn clone(&self) -> Self {
        Self {
            commands: self.commands.clone(),
            join: None,
        }
    }
}

impl SftpHandle {
    pub fn list_dir(&self, path: String) {
        let _ = self.commands.send(SftpCommand::ListDir(path));
    }

    #[allow(dead_code)]
    pub fn preview(&self, path: String) {
        let _ = self.commands.send(SftpCommand::Preview(path));
    }

    pub fn download(&self, remote: String, local_dir: String) {
        let _ = self
            .commands
            .send(SftpCommand::Download { remote, local_dir });
    }

    pub fn upload_paths(&self, locals: Vec<String>, remote_dir: String) {
        let _ = self
            .commands
            .send(SftpCommand::UploadPaths { locals, remote_dir });
    }

    pub fn read_text_file(
        &self,
        remote_path: String,
    ) -> oneshot::Receiver<std::result::Result<Vec<u8>, String>> {
        let (reply, response) = oneshot::channel();
        let _ = self
            .commands
            .send(SftpCommand::ReadTextFile { remote_path, reply });
        response
    }

    pub fn write_text_file(
        &self,
        remote_path: String,
        content: Vec<u8>,
    ) -> oneshot::Receiver<std::result::Result<(), String>> {
        let (reply, response) = oneshot::channel();
        let _ = self.commands.send(SftpCommand::WriteTextFile {
            remote_path,
            content,
            reply,
        });
        response
    }

    pub fn rename_path(
        &self,
        old_path: String,
        new_path: String,
    ) -> oneshot::Receiver<std::result::Result<(), String>> {
        let (reply, response) = oneshot::channel();
        let _ = self.commands.send(SftpCommand::RenamePath {
            old_path,
            new_path,
            reply,
        });
        response
    }

    pub fn close(&self) {
        let _ = self.commands.send(SftpCommand::Close);
    }

    pub fn pause_transfer(&self, id: String) {
        let _ = self.commands.send(SftpCommand::PauseTransfer(id));
    }

    pub fn resume_transfer(&self, id: String) {
        let _ = self.commands.send(SftpCommand::ResumeTransfer(id));
    }

    pub fn cancel_transfer(&self, id: String) {
        let _ = self.commands.send(SftpCommand::CancelTransfer(id));
    }
}

pub fn spawn_sftp(
    runtime: &tokio::runtime::Handle,
    tab_id: String,
    session: Session,
    events: std::sync::mpsc::Sender<BackendEvent>,
) -> SftpHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let cmd_tx_clone = cmd_tx.clone();
    let join = runtime.spawn(async move {
        if let Err(err) = run_sftp(
            tab_id.clone(),
            session,
            cmd_rx,
            cmd_tx_clone,
            events.clone(),
        )
        .await
        {
            let _ = events.send(BackendEvent::SftpStatus {
                tab_id: tab_id.clone(),
                text: format!("sftp error: {err:#}"),
            });
        }
    });
    SftpHandle {
        commands: cmd_tx,
        join: Some(join),
    }
}

async fn run_sftp(
    tab_id: String,
    session: Session,
    mut commands: UnboundedReceiver<SftpCommand>,
    commands_tx: UnboundedSender<SftpCommand>,
    events: std::sync::mpsc::Sender<BackendEvent>,
) -> Result<()> {
    let _ = events.send(BackendEvent::SftpStatus {
        tab_id: tab_id.clone(),
        text: t!("sftp_connecting").to_string(),
    });

    let handle = connect_and_authenticate(&session).await?;
    let sftp = open_sftp_session(&handle).await?;

    let home = sftp
        .canonicalize(".")
        .await
        .unwrap_or_else(|_| "/".to_string());

    let _ = events.send(BackendEvent::SftpHome {
        tab_id: tab_id.clone(),
        home: home.clone(),
    });

    emit_entries(&events, &tab_id, &sftp, &home).await?;

    let mut active_transfers: std::collections::HashMap<String, TransferStateFlag> =
        std::collections::HashMap::new();

    while let Some(command) = commands.recv().await {
        match command {
            SftpCommand::Close => break,
            SftpCommand::PauseTransfer(id) => {
                if let Some(flag) = active_transfers.get(&id) {
                    flag.pause();
                }
            }
            SftpCommand::ResumeTransfer(id) => {
                if let Some(flag) = active_transfers.get(&id) {
                    flag.resume();
                }
            }
            SftpCommand::CancelTransfer(id) => {
                if let Some(flag) = active_transfers.remove(&id) {
                    flag.cancel();
                }
            }
            SftpCommand::TransferFinished(id) => {
                active_transfers.remove(&id);
            }
            SftpCommand::ListDir(path) => {
                let actual_path = if path == "~" {
                    home.clone()
                } else if let Some(rest) = path.strip_prefix("~/") {
                    crate::sftp::join_remote(&home, rest)
                } else {
                    path
                };

                if let Err(err) = emit_entries(&events, &tab_id, &sftp, &actual_path).await {
                    let reason = format!("list failed: {err:#}");
                    let _ = events.send(BackendEvent::SftpDirectoryFailed {
                        tab_id: tab_id.clone(),
                        path: actual_path,
                        reason: reason.clone(),
                    });
                    let _ = events.send(BackendEvent::SftpStatus {
                        tab_id: tab_id.clone(),
                        text: reason,
                    });
                }
            }
            SftpCommand::Preview(path) => match preview_impl(&sftp, &path).await {
                Ok(preview) => {
                    let _ = events.send(BackendEvent::SftpPreview {
                        tab_id: tab_id.clone(),
                        preview,
                    });
                }
                Err(err) => {
                    let _ = events.send(BackendEvent::SftpStatus {
                        tab_id: tab_id.clone(),
                        text: t!("preview_failed", err = format!("{err:#}")).into(),
                    });
                }
            },
            SftpCommand::Download { remote, local_dir } => {
                let id = uuid::Uuid::new_v4().to_string();
                let flag = TransferStateFlag::new();
                active_transfers.insert(id.clone(), TransferStateFlag(flag.0.clone()));

                let info = crate::terminal::TransferInfo {
                    id: id.clone(),
                    name: base_name(&remote).to_string(),
                    source: remote.clone(),
                    target: local_dir.clone(),
                    kind: crate::terminal::TransferType::Download,
                    total_bytes: None,
                };
                let _ = events.send(BackendEvent::TransferStarted {
                    tab_id: tab_id.clone(),
                    info,
                });

                let handle_clone = handle.clone();
                let events_clone = events.clone();
                let tab_id_clone = tab_id.clone();
                let commands_tx_clone = commands_tx.clone();

                tokio::spawn(async move {
                    let result = async {
                        let sftp_session = open_sftp_session(&handle_clone).await?;
                        let _ = events_clone.send(BackendEvent::SftpStatus {
                            tab_id: tab_id_clone.clone(),
                            text: t!("downloading_file", base = base_name(&remote)).to_string(),
                        });
                        let transfer = TransferContext {
                            flag: &flag,
                            events: &events_clone,
                            tab_id: &tab_id_clone,
                            id: &id,
                        };
                        download_path_impl(
                            &handle_clone,
                            &sftp_session,
                            &remote,
                            Path::new(&local_dir),
                            transfer,
                        )
                        .await
                    }
                    .await;

                    match result {
                        Ok(summary) => {
                            let _ = events_clone.send(BackendEvent::SftpStatus {
                                tab_id: tab_id_clone,
                                text: summary,
                            });
                        }
                        Err(err) => {
                            let err_msg = format!("{err:#}");
                            let is_cancelled = err_msg.contains("transfer cancelled");
                            let state = if is_cancelled {
                                crate::terminal::TransferState::Interrupted(
                                    "User cancelled".to_string(),
                                )
                            } else {
                                crate::terminal::TransferState::Failed(err_msg.clone())
                            };
                            let _ = events_clone.send(BackendEvent::SftpStatus {
                                tab_id: tab_id_clone.clone(),
                                text: if is_cancelled {
                                    "Transmission cancelled".to_string()
                                } else {
                                    t!("download_failed", err = err_msg.clone()).to_string()
                                },
                            });
                            let _ = events_clone.send(BackendEvent::TransferProgress {
                                tab_id: tab_id_clone,
                                id: id.clone(),
                                transferred: 0,
                                total: None,
                                state,
                            });
                        }
                    }
                    let _ = commands_tx_clone.send(SftpCommand::TransferFinished(id));
                });
            }
            SftpCommand::UploadPaths { locals, remote_dir } => {
                let id = uuid::Uuid::new_v4().to_string();
                let flag = TransferStateFlag::new();
                active_transfers.insert(id.clone(), TransferStateFlag(flag.0.clone()));

                let name = if locals.len() == 1 {
                    base_name(&locals[0]).to_string()
                } else {
                    let mut file_count = 0;
                    let mut folder_count = 0;
                    for local in &locals {
                        if std::path::Path::new(local).is_dir() {
                            folder_count += 1;
                        } else {
                            file_count += 1;
                        }
                    }
                    if file_count > 0 && folder_count == 0 {
                        t!("n_files", files = file_count).to_string()
                    } else if file_count == 0 && folder_count > 0 {
                        t!("n_folders", folders = folder_count).to_string()
                    } else {
                        t!(
                            "n_files_and_folders",
                            files = file_count,
                            folders = folder_count
                        )
                        .to_string()
                    }
                };

                let info = crate::terminal::TransferInfo {
                    id: id.clone(),
                    name,
                    source: "local".to_string(),
                    target: remote_dir.clone(),
                    kind: crate::terminal::TransferType::Upload,
                    total_bytes: None,
                };
                let _ = events.send(BackendEvent::TransferStarted {
                    tab_id: tab_id.clone(),
                    info,
                });

                let handle_clone = handle.clone();
                let events_clone = events.clone();
                let tab_id_clone = tab_id.clone();
                let commands_tx_clone = commands_tx.clone();

                tokio::spawn(async move {
                    let result = async {
                        let sftp_session = open_sftp_session(&handle_clone).await?;
                        let _ = events_clone.send(BackendEvent::SftpStatus {
                            tab_id: tab_id_clone.clone(),
                            text: t!("uploading").to_string(),
                        });
                        upload_paths_impl(
                            &sftp_session,
                            &locals,
                            &remote_dir,
                            flag,
                            &events_clone,
                            &tab_id_clone,
                            &id,
                        )
                        .await
                    }
                    .await;

                    match result {
                        Ok(summary) => {
                            let _ = events_clone.send(BackendEvent::SftpStatus {
                                tab_id: tab_id_clone.clone(),
                                text: summary,
                            });
                            let _ = commands_tx_clone.send(SftpCommand::ListDir(remote_dir));
                        }
                        Err(err) => {
                            let err_msg = format!("{err:#}");
                            let is_cancelled = err_msg.contains("transfer cancelled");
                            let state = if is_cancelled {
                                crate::terminal::TransferState::Interrupted(
                                    "User cancelled".to_string(),
                                )
                            } else {
                                crate::terminal::TransferState::Failed(err_msg.clone())
                            };
                            let _ = events_clone.send(BackendEvent::SftpStatus {
                                tab_id: tab_id_clone.clone(),
                                text: if is_cancelled {
                                    "Transmission cancelled".to_string()
                                } else {
                                    t!("upload_failed", err = err_msg.clone()).to_string()
                                },
                            });
                            let _ = events_clone.send(BackendEvent::TransferProgress {
                                tab_id: tab_id_clone,
                                id: id.clone(),
                                transferred: 0,
                                total: None,
                                state,
                            });
                        }
                    }
                    let _ = commands_tx_clone.send(SftpCommand::TransferFinished(id));
                });
            }
            SftpCommand::ReadTextFile { remote_path, reply } => {
                let result = read_text_file_impl(&sftp, &remote_path)
                    .await
                    .map_err(|err| format!("{err:#}"));
                let _ = reply.send(result);
            }
            SftpCommand::WriteTextFile {
                remote_path,
                content,
                reply,
            } => {
                let result = write_text_file_impl(&sftp, &remote_path, &content)
                    .await
                    .map_err(|err| format!("{err:#}"));
                if result.is_ok() {
                    if let Some(parent) = parent_dir(&remote_path) {
                        let _ = commands_tx.send(SftpCommand::ListDir(parent));
                    }
                }
                let _ = reply.send(result);
            }
            SftpCommand::RenamePath {
                old_path,
                new_path,
                reply,
            } => {
                let result = rename_path_impl(&sftp, &old_path, &new_path)
                    .await
                    .map_err(|err| format!("{err:#}"));
                if result.is_ok() {
                    if let Some(parent) = parent_dir(&old_path) {
                        let _ = commands_tx.send(SftpCommand::ListDir(parent));
                    }
                }
                let _ = reply.send(result);
            }
            SftpCommand::CreateDir(path) => {
                let actual_path = if path == "~" {
                    home.clone()
                } else if let Some(rest) = path.strip_prefix("~/") {
                    crate::sftp::join_remote(&home, rest)
                } else {
                    path.clone()
                };

                tracing::info!("[sftp] creating directory: '{}'", actual_path);

                match sftp.create_dir(&actual_path).await {
                    Ok(_) => {
                        let _ = events.send(BackendEvent::SftpStatus {
                            tab_id: tab_id.clone(),
                            text: t!("create_folder_success", name = base_name(&actual_path))
                                .to_string(),
                        });

                        // Re-fetch the parent directory to show the newly created folder
                        if let Some(parent) = parent_dir(&actual_path) {
                            let _ = commands_tx.send(SftpCommand::ListDir(parent));
                        } else {
                            let _ = commands_tx.send(SftpCommand::ListDir("/".to_string()));
                        }
                    }
                    Err(err) => {
                        let _ = events.send(BackendEvent::SftpStatus {
                            tab_id: tab_id.clone(),
                            text: t!("create_folder_failed", err = format!("{err:#}")).to_string(),
                        });
                    }
                }
            }
            SftpCommand::DeletePaths(paths) => {
                tracing::info!("[sftp] batch deleting {} paths", paths.len());
                let _ = events.send(BackendEvent::SftpStatus {
                    tab_id: tab_id.clone(),
                    text: t!("deleting_paths", count = paths.len()).to_string(),
                });

                let mut errors = Vec::new();
                for path in paths.clone() {
                    let actual_path = if path == "~" {
                        home.clone()
                    } else if let Some(rest) = path.strip_prefix("~/") {
                        crate::sftp::join_remote(&home, rest)
                    } else {
                        path.clone()
                    };

                    if let Err(e) = recursive_delete(&sftp, actual_path).await {
                        errors.push(format!("{path}: {e:#}"));
                    }
                }

                if errors.is_empty() {
                    let _ = events.send(BackendEvent::SftpStatus {
                        tab_id: tab_id.clone(),
                        text: t!("delete_success", count = paths.len()).to_string(),
                    });
                } else {
                    let _ = events.send(BackendEvent::SftpStatus {
                        tab_id: tab_id.clone(),
                        text: t!("delete_failed", err = errors.join(", ")).to_string(),
                    });
                }

                if let Some(first) = paths.first() {
                    let actual_path = if first == "~" {
                        home.clone()
                    } else if let Some(rest) = first.strip_prefix("~/") {
                        crate::sftp::join_remote(&home, rest)
                    } else {
                        first.clone()
                    };
                    if let Some(parent) = parent_dir(&actual_path) {
                        let _ = commands_tx.send(SftpCommand::ListDir(parent));
                    } else {
                        let _ = commands_tx.send(SftpCommand::ListDir("/".to_string()));
                    }
                }
            }
        }
    }

    let _ = handle
        .disconnect(Disconnect::ByApplication, "bye", "")
        .await;
    Ok(())
}

async fn open_sftp_session(
    handle: &russh::client::Handle<SftpClientHandler>,
) -> Result<SftpSession> {
    let channel = handle
        .channel_open_session()
        .await
        .context("open sftp channel")?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .context("request sftp subsystem")?;
    SftpSession::new(channel.into_stream())
        .await
        .context("sftp handshake")
}

use std::future::Future;
use std::pin::Pin;

fn recursive_delete<'a>(
    sftp: &'a SftpSession,
    path: String,
) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(async move {
        match sftp.read_dir(&path).await {
            Ok(entries) => {
                for entry in entries {
                    let name = entry.file_name();
                    if name == "." || name == ".." {
                        continue;
                    }
                    let child_path = crate::sftp::join_remote(&path, &name);

                    let meta = entry.metadata();
                    let permissions = meta.permissions.unwrap_or(0);
                    let is_dir = (permissions & 0o170_000) == 0o040_000;

                    if is_dir {
                        recursive_delete(sftp, child_path).await?;
                    } else {
                        sftp.remove_file(&child_path)
                            .await
                            .with_context(|| format!("Failed to delete file {child_path}"))?;
                    }
                }
                sftp.remove_dir(&path)
                    .await
                    .with_context(|| format!("Failed to delete dir {path}"))?;
            }
            Err(_) => {
                sftp.remove_file(&path)
                    .await
                    .with_context(|| format!("Failed to delete {path}"))?;
            }
        }
        Ok(())
    })
}

async fn emit_entries(
    events: &std::sync::mpsc::Sender<BackendEvent>,
    tab_id: &str,
    sftp: &SftpSession,
    path: &str,
) -> Result<()> {
    let entries = list_dir_impl(sftp, path).await?;
    let _ = events.send(BackendEvent::SftpEntries {
        tab_id: tab_id.to_string(),
        path: path.to_string(),
        entries,
    });
    let _ = events.send(BackendEvent::SftpStatus {
        tab_id: tab_id.to_string(),
        text: path.to_string(),
    });
    Ok(())
}

async fn connect_and_authenticate(
    session: &Session,
) -> Result<Arc<russh::client::Handle<SftpClientHandler>>> {
    let config = Arc::new(crate::session::config::ssh_client_config());
    let addr = format!("{}:{}", session.host, session.port);
    let stream = crate::session::config::connect_proxy(session).await?;
    let mut handle = client::connect_stream(config, stream, SftpClientHandler)
        .await
        .with_context(|| format!("connect {addr} failed"))?;

    let authed = match session.auth {
        AuthMethod::Password => handle
            .authenticate_password(&session.user, &session.password)
            .await
            .context("password authentication failed")?,
        AuthMethod::Key => {
            let has_explicit_key = session_has_explicit_key(session);
            if has_explicit_key {
                let keypair = load_session_private_key(session)?;
                let keys = private_keys_with_algs(keypair).context("invalid private key")?;
                let mut success = false;
                for key in keys {
                    match handle.authenticate_publickey(&session.user, key).await {
                        Ok(true) => {
                            success = true;
                            break;
                        }
                        Ok(false) => {
                            tracing::debug!(
                                "[sftp] public key auth failed with algorithm, trying next"
                            );
                            continue;
                        }
                        Err(e) => {
                            tracing::debug!("[sftp] public key auth error: {:?}, trying next", e);
                            continue;
                        }
                    }
                }
                if !success {
                    return Err(anyhow!(
                        "public key authentication failed for {}@{}:{}",
                        session.user,
                        session.host,
                        session.port
                    ));
                }
                success
            } else {
                let passphrase = session.passphrase.trim();
                let passphrase = (!passphrase.is_empty()).then_some(passphrase);
                let success =
                    authenticate_with_default_keys(&mut handle, &session.user, passphrase).await?;
                if !success {
                    return Err(anyhow!(
                        "public key authentication failed for {}@{}:{} - no valid default key found in ~/.ssh/",
                        session.user,
                        session.host,
                        session.port
                    ));
                }
                success
            }
        }
        AuthMethod::Config => {
            // For Config auth, try the identity file from config entry, or default keys
            // Note: for Config auth, we never use inline key content
            let has_explicit_key = !session.private_key_path.trim().is_empty();

            if has_explicit_key {
                let keypair = load_session_private_key(session)?;
                let keys = private_keys_with_algs(keypair).context("invalid private key")?;
                let mut success = false;
                for key in keys {
                    match handle.authenticate_publickey(&session.user, key).await {
                        Ok(true) => {
                            success = true;
                            break;
                        }
                        Ok(false) => {
                            tracing::debug!(
                                "[sftp] public key auth failed with algorithm, trying next"
                            );
                            continue;
                        }
                        Err(e) => {
                            tracing::debug!("[sftp] public key auth error: {:?}, trying next", e);
                            continue;
                        }
                    }
                }
                if !success {
                    return Err(anyhow!(
                        "ssh-config key authentication failed for {}@{}:{}",
                        session.user,
                        session.host,
                        session.port
                    ));
                }
                success
            } else {
                let passphrase = session.passphrase.trim();
                let passphrase = (!passphrase.is_empty()).then_some(passphrase);
                let success =
                    authenticate_with_default_keys(&mut handle, &session.user, passphrase).await?;
                if !success {
                    return Err(anyhow!(
                        "ssh-config authentication failed for {}@{}:{} - no valid default key found",
                        session.user,
                        session.host,
                        session.port
                    ));
                }
                success
            }
        }
    };

    if !authed {
        let _ = handle
            .disconnect(Disconnect::ByApplication, "auth failed", "")
            .await;
        return Err(anyhow!(
            "authentication failed: server rejected {} authentication for {}@{}:{}",
            match session.auth {
                AuthMethod::Password => "password",
                AuthMethod::Key => "public key",
                AuthMethod::Config => "ssh-config",
            },
            session.user,
            session.host,
            session.port
        ));
    }

    Ok(Arc::new(handle))
}

fn load_session_private_key(session: &Session) -> Result<PrivateKey> {
    let inline_key = normalize_inline_private_key(&session.private_key_inline);
    let key_path = expand_key_path(session.private_key_path.trim());
    let passphrase = session.passphrase.trim();
    let passphrase = (!passphrase.is_empty()).then_some(passphrase);
    let has_inline = !inline_key.is_empty();
    let has_path = key_path.is_some();

    if !has_inline && !has_path {
        return Err(anyhow!("private key content or path is required"));
    }

    let mut errors = Vec::new();

    if has_inline {
        match decode_secret_key(&inline_key, passphrase) {
            Ok(key) => return Ok(key),
            Err(err) => errors.push(format!("decode private key content: {err}")),
        }
    }

    if let Some(path) = key_path {
        match load_secret_key(path.as_path(), passphrase) {
            Ok(key) => return Ok(key),
            Err(err) => errors.push(format!("load key {}: {err}", path.display())),
        }
    }

    Err(anyhow!(errors.join("; ")))
}

fn expand_key_path(value: &str) -> Option<PathBuf> {
    if value.is_empty() {
        return None;
    }
    if value == "~" {
        return BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf());
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return BaseDirs::new().map(|dirs| dirs.home_dir().join(rest));
    }
    Some(Path::new(value).to_path_buf())
}

pub(crate) fn base_name(path: &str) -> String {
    let sep = |c: char| c == '/' || c == '\\';
    path.trim_end_matches(sep)
        .rsplit(sep)
        .next()
        .unwrap_or(path)
        .to_string()
}

pub(crate) fn editor_language(path: &str) -> &'static str {
    let name = base_name(path).to_lowercase();
    if name == "dockerfile" {
        return "bash";
    }
    if name == "makefile" {
        return "make";
    }

    match Path::new(&name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
    {
        "bash" | "zsh" | "sh" => "bash",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" => "cpp",
        "cs" => "csharp",
        "css" | "scss" => "css",
        "go" => "go",
        "html" | "htm" => "html",
        "java" => "java",
        "js" | "mjs" | "cjs" => "javascript",
        "json" | "jsonc" => "json",
        "kt" | "kts" => "kotlin",
        "lua" => "lua",
        "md" | "markdown" => "markdown",
        "php" => "php",
        "proto" => "proto",
        "py" => "python",
        "rb" => "ruby",
        "rs" => "rust",
        "sql" => "sql",
        "svelte" => "svelte",
        "swift" => "swift",
        "toml" => "toml",
        "ts" => "typescript",
        "tsx" => "tsx",
        "yaml" | "yml" => "yaml",
        "zig" => "zig",
        _ => "text",
    }
}

pub(crate) fn parent_dir(path: &str) -> Option<String> {
    if path == "/" || path.is_empty() {
        return None;
    }
    let trimmed = path.trim_end_matches('/');
    if let Some(idx) = trimmed.rfind('/') {
        if idx == 0 {
            Some("/".to_string())
        } else {
            Some(trimmed[..idx].to_string())
        }
    } else {
        Some("/".to_string())
    }
}

pub(crate) fn join_remote(parent: &str, child: &str) -> String {
    if parent == "/" {
        format!("/{child}")
    } else {
        format!("{}/{}", parent.trim_end_matches('/'), child)
    }
}

pub(crate) fn normalize_remote_path(input: &str, current: &str, home: &str) -> String {
    let input = input.trim();
    let expanded = if input.is_empty() {
        current.to_string()
    } else if input == "~" {
        home.to_string()
    } else if let Some(rest) = input.strip_prefix("~/") {
        join_remote(home, rest)
    } else if input.starts_with('/') {
        input.to_string()
    } else {
        join_remote(current, input)
    };

    let mut components = Vec::new();
    for component in expanded.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            value => components.push(value),
        }
    }

    if components.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", components.join("/"))
    }
}

pub(crate) fn remote_path_ancestors(path: &str) -> Vec<String> {
    let normalized = normalize_remote_path(path, "/", "/");
    let mut ancestors = vec!["/".to_string()];
    let mut current = String::new();
    for component in normalized.trim_start_matches('/').split('/') {
        if component.is_empty() {
            continue;
        }
        current.push('/');
        current.push_str(component);
        ancestors.push(current.clone());
    }
    ancestors
}

#[allow(dead_code)]
fn strip_archive_suffix(name: &str) -> &str {
    for suffix in [".tar.gz", ".tgz", ".zip", ".tar"] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            return stripped;
        }
    }
    name
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

pub fn format_mtime(ts: u32) -> String {
    let dt: DateTime<Utc> = Utc
        .timestamp_opt(ts as i64, 0)
        .single()
        .unwrap_or_else(Utc::now);
    dt.format("%Y-%m-%d %H:%M").to_string()
}

async fn list_dir_impl(sftp: &SftpSession, path: &str) -> Result<Vec<RemoteEntry>> {
    let raw = sftp
        .read_dir(path)
        .await
        .with_context(|| format!("read_dir {path} failed"))?;

    let mut entries = raw
        .into_iter()
        .filter(|entry| {
            let name = entry.file_name();
            name != "." && name != ".."
        })
        .map(|entry| {
            let name = entry.file_name().to_string();
            let full_path = join_remote(path, &name);
            let meta = entry.metadata();
            let permissions = meta.permissions.unwrap_or(0);
            let is_dir = (permissions & 0o170_000) == 0o040_000;
            let size = meta.size.unwrap_or(0);
            let modified = meta.mtime.unwrap_or(0);
            RemoteEntry {
                name,
                full_path,
                is_dir,
                size,
                modified,
            }
        })
        .collect::<Vec<_>>();

    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });

    Ok(entries)
}

pub(crate) const MAX_INLINE_EDIT_BYTES: usize = 2 * 1024 * 1024;

async fn read_text_file_impl(sftp: &SftpSession, path: &str) -> Result<Vec<u8>> {
    let metadata = sftp
        .metadata(path)
        .await
        .with_context(|| format!("read metadata for {path}"))?;
    if metadata.size.unwrap_or(0) > MAX_INLINE_EDIT_BYTES as u64 {
        return Err(anyhow!("file exceeds the 2 MB in-app editing limit"));
    }

    let remote_file = sftp
        .open(path)
        .await
        .with_context(|| format!("open remote {path}"))?;
    let mut limited = remote_file.take((MAX_INLINE_EDIT_BYTES + 1) as u64);
    let mut content = Vec::new();
    limited
        .read_to_end(&mut content)
        .await
        .with_context(|| format!("read remote {path}"))?;
    if content.len() > MAX_INLINE_EDIT_BYTES {
        return Err(anyhow!("file exceeds the 2 MB in-app editing limit"));
    }

    Ok(content)
}

async fn write_text_file_impl(sftp: &SftpSession, path: &str, content: &[u8]) -> Result<()> {
    if content.len() > MAX_INLINE_EDIT_BYTES {
        return Err(anyhow!("file exceeds the 2 MB in-app editing limit"));
    }

    let permissions = sftp
        .metadata(path)
        .await
        .ok()
        .and_then(|meta| meta.permissions);
    let temporary_path = format!("{path}.ashell-{}.tmp", Uuid::new_v4());
    let write_result = async {
        let mut remote_file = sftp
            .create(temporary_path.as_str())
            .await
            .with_context(|| format!("open remote temporary file {temporary_path}"))?;
        remote_file
            .write_all(content)
            .await
            .with_context(|| format!("write remote temporary file {temporary_path}"))?;
        remote_file
            .flush()
            .await
            .with_context(|| format!("flush remote temporary file {temporary_path}"))?;
        drop(remote_file);

        if let Some(permissions) = permissions {
            sftp.set_metadata(
                temporary_path.as_str(),
                FileAttributes {
                    permissions: Some(permissions),
                    ..FileAttributes::default()
                },
            )
            .await
            .with_context(|| format!("preserve permissions for {path}"))?;
        }

        sftp.rename(temporary_path.as_str(), path)
            .await
            .with_context(|| format!("replace remote {path}"))
    }
    .await;

    if let Err(err) = write_result {
        let _ = sftp.remove_file(temporary_path.as_str()).await;
        return Err(err);
    }
    Ok(())
}

async fn rename_path_impl(sftp: &SftpSession, old_path: &str, new_path: &str) -> Result<()> {
    if old_path == new_path {
        return Ok(());
    }
    if sftp.metadata(new_path).await.is_ok() {
        return Err(anyhow!("target already exists: {new_path}"));
    }
    sftp.rename(old_path, new_path)
        .await
        .with_context(|| format!("rename {old_path} to {new_path}"))
}

async fn preview_impl(sftp: &SftpSession, path: &str) -> Result<PreviewData> {
    let metadata = sftp
        .metadata(path)
        .await
        .with_context(|| format!("metadata {path}"))?;
    let is_dir = metadata
        .permissions
        .map(|mode| (mode & 0o170_000) == 0o040_000)
        .unwrap_or(false);

    if is_dir {
        let entries = list_dir_impl(sftp, path).await?;
        let mut lines = vec![format!("Directory: {path}"), String::new()];
        for entry in entries.into_iter().take(200) {
            let kind = if entry.is_dir { "dir " } else { "file" };
            lines.push(format!("{kind}  {}", entry.name));
        }
        return Ok(PreviewData {
            path: path.to_string(),
            title: base_name(path),
            body: lines.join("\n"),
            is_binary: false,
        });
    }

    let mut remote_file = sftp
        .open(path)
        .await
        .with_context(|| format!("open remote {path}"))?;
    let mut buffer = vec![0u8; 128 * 1024];
    let read = remote_file
        .read(&mut buffer)
        .await
        .context("read preview bytes")?;
    buffer.truncate(read);

    let nul_ratio = if buffer.is_empty() {
        0.0
    } else {
        buffer.iter().filter(|byte| **byte == 0).count() as f32 / buffer.len() as f32
    };
    let is_binary = nul_ratio > 0.01;
    let body = if is_binary {
        format!(
            "Binary file\npath: {path}\nsize: {}\npreview: unavailable in-app",
            format_bytes(metadata.size.unwrap_or(0)),
        )
    } else {
        String::from_utf8_lossy(&buffer).into_owned()
    };

    Ok(PreviewData {
        path: path.to_string(),
        title: base_name(path),
        body,
        is_binary,
    })
}

async fn download_path_impl(
    handle: &russh::client::Handle<SftpClientHandler>,
    sftp: &SftpSession,
    remote: &str,
    local_dir: &Path,
    transfer: TransferContext<'_>,
) -> Result<String> {
    tokio::fs::create_dir_all(local_dir)
        .await
        .with_context(|| format!("create {}", local_dir.display()))?;

    // Check for cancellation after initial setup
    let state = transfer.flag.0.load(Ordering::SeqCst);
    if state == 2 {
        return Err(anyhow::anyhow!("transfer cancelled"));
    }

    let metadata = sftp
        .metadata(remote)
        .await
        .with_context(|| format!("metadata {remote}"))?;
    let is_dir = metadata
        .permissions
        .map(|mode| (mode & 0o170_000) == 0o040_000)
        .unwrap_or(false);

    if is_dir {
        let local_archive = local_dir.join(format!(
            ".ashell-{}-{}.tar.gz",
            base_name(remote),
            Uuid::new_v4()
        ));
        let extracted_to =
            download_remote_directory_archive(handle, sftp, remote, &local_archive, transfer)
                .await?;
        return Ok(t!("downloaded_folder", path = extracted_to.display()).to_string());
    }

    let local_path = local_dir.join(base_name(remote));
    download_file_impl(sftp, remote, &local_path, transfer).await?;
    Ok(t!("downloaded_file", path = local_path.display()).to_string())
}

#[allow(dead_code)]
async fn download_dir_recursive(
    sftp: &SftpSession,
    remote_dir: &str,
    local_dir: &Path,
    transfer: TransferContext<'_>,
) -> Result<()> {
    tokio::fs::create_dir_all(local_dir)
        .await
        .with_context(|| format!("create {}", local_dir.display()))?;
    let entries = list_dir_impl(sftp, remote_dir).await?;
    for entry in entries {
        let local_path = local_dir.join(&entry.name);
        if entry.is_dir {
            Box::pin(download_dir_recursive(
                sftp,
                &entry.full_path,
                &local_path,
                transfer,
            ))
            .await?;
        } else {
            download_file_impl(sftp, &entry.full_path, &local_path, transfer).await?;
            let _ = maybe_extract_archive(&local_path).await;
        }
    }
    Ok(())
}

async fn download_remote_directory_archive(
    handle: &russh::client::Handle<SftpClientHandler>,
    sftp: &SftpSession,
    remote_dir: &str,
    local_archive: &Path,
    transfer: TransferContext<'_>,
) -> Result<PathBuf> {
    let remote_archive = format!(
        "/tmp/ashell-{}-{}.tar.gz",
        base_name(remote_dir),
        Uuid::new_v4()
    );

    // Check for cancellation before creating remote archive
    let state = transfer.flag.0.load(Ordering::SeqCst);
    if state == 2 {
        return Err(anyhow::anyhow!("transfer cancelled"));
    }

    create_remote_archive(handle, remote_dir, &remote_archive).await?;

    let local_extract_root = local_archive
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(base_name(remote_dir));

    let archive_download = async {
        download_file_impl(sftp, &remote_archive, local_archive, transfer).await?;
        extract_archive_to(
            local_archive,
            local_archive.parent().unwrap_or_else(|| Path::new(".")),
        )
        .await?;
        tokio::fs::remove_file(local_archive)
            .await
            .with_context(|| format!("remove {}", local_archive.display()))?;
        Ok::<PathBuf, anyhow::Error>(local_extract_root)
    }
    .await;

    let cleanup_result = remove_remote_path(handle, &remote_archive).await;

    let extracted_to = archive_download?;
    if let Err(err) = cleanup_result {
        tracing::warn!("failed to clean remote archive {remote_archive}: {err:#}");
    }

    Ok(extracted_to)
}

async fn download_file_impl(
    sftp: &SftpSession,
    remote: &str,
    local: &Path,
    transfer: TransferContext<'_>,
) -> Result<()> {
    let mut remote_file = sftp
        .open(remote)
        .await
        .with_context(|| format!("open remote {remote}"))?;
    let mut local_file = tokio::fs::File::create(local)
        .await
        .with_context(|| format!("create local {}", local.display()))?;

    let total = sftp.metadata(remote).await.ok().and_then(|m| m.size);
    let mut transferred = 0u64;

    let mut buffer = vec![0u8; 128 * 1024];
    loop {
        transfer
            .flag
            .yield_if_paused(
                transfer.events,
                transfer.tab_id,
                transfer.id,
                transferred,
                total,
            )
            .await?;
        let read = remote_file
            .read(&mut buffer)
            .await
            .context("read remote file")?;
        if read == 0 {
            break;
        }
        local_file
            .write_all(&buffer[..read])
            .await
            .with_context(|| format!("write {}", local.display()))?;

        transferred += read as u64;
        let _ = transfer.events.send(BackendEvent::TransferProgress {
            tab_id: transfer.tab_id.to_string(),
            id: transfer.id.to_string(),
            transferred,
            total,
            state: crate::terminal::TransferState::Running,
        });
    }
    local_file.flush().await.context("flush local file")?;

    let _ = transfer.events.send(BackendEvent::TransferProgress {
        tab_id: transfer.tab_id.to_string(),
        id: transfer.id.to_string(),
        transferred,
        total,
        state: crate::terminal::TransferState::Completed,
    });

    Ok(())
}

async fn upload_paths_impl(
    sftp: &SftpSession,
    locals: &[String],
    remote_dir: &str,
    flag: TransferStateFlag,
    events: &std::sync::mpsc::Sender<BackendEvent>,
    tab_id: &str,
    id: &str,
) -> Result<String> {
    // Check for cancellation before starting
    let state = flag.0.load(Ordering::SeqCst);
    if state == 2 {
        return Err(anyhow::anyhow!("transfer cancelled"));
    }

    create_remote_dir_all(sftp, remote_dir).await?;
    let mut file_count = 0usize;
    let mut folder_count = 0usize;

    let mut total_bytes = 0u64;
    let mut files_to_upload = Vec::new();
    let mut dirs_to_create = Vec::new();

    for local in locals {
        let p = PathBuf::from(local);
        if p.is_dir() {
            folder_count += 1;
            let root_name = p.file_name().and_then(|n| n.to_str()).unwrap_or("folder");
            let remote_root = join_remote(remote_dir, root_name);
            dirs_to_create.push(remote_root.clone());

            for entry in WalkDir::new(&p) {
                let entry = entry?;
                let path = entry.path();
                if path == p {
                    continue;
                }

                if let Ok(meta) = tokio::fs::metadata(&path).await {
                    let relative = path.strip_prefix(&p)?;
                    let remote_path = if relative.as_os_str().is_empty() {
                        remote_root.clone()
                    } else {
                        let rel = relative
                            .components()
                            .map(|c| c.as_os_str().to_string_lossy().to_string())
                            .collect::<Vec<_>>()
                            .join("/");
                        join_remote(&remote_root, &rel)
                    };

                    if path.is_dir() {
                        dirs_to_create.push(remote_path);
                    } else {
                        total_bytes += meta.len();
                        files_to_upload.push((path.to_path_buf(), remote_path));
                    }
                }
            }
        } else if let Ok(meta) = tokio::fs::metadata(&p).await {
            total_bytes += meta.len();
            let file_name = p.file_name().and_then(|n| n.to_str()).unwrap_or("file");
            files_to_upload.push((p.clone(), join_remote(remote_dir, file_name)));
            file_count += 1;
        }
    }

    // Check for cancellation before creating directories
    let state = flag.0.load(Ordering::SeqCst);
    if state == 2 {
        return Err(anyhow::anyhow!("transfer cancelled"));
    }

    // Create directories sequentially first
    for dir in dirs_to_create {
        // Check for cancellation between each directory creation
        let state = flag.0.load(Ordering::SeqCst);
        if state == 2 {
            return Err(anyhow::anyhow!("transfer cancelled"));
        }
        create_remote_dir_all(sftp, &dir).await?;
    }

    let transferred = Arc::new(AtomicU64::new(0));
    let mut futures = Vec::new();

    for (local_path, remote_path) in files_to_upload {
        let flag_clone = TransferStateFlag(Arc::clone(&flag.0));
        let events_clone = events.clone();
        let tab_id_clone = tab_id.to_string();
        let id_clone = id.to_string();
        let transferred_clone = Arc::clone(&transferred);

        futures.push(async move {
            let transfer = TransferContext {
                flag: &flag_clone,
                events: &events_clone,
                tab_id: &tab_id_clone,
                id: &id_clone,
            };
            upload_file_impl(
                sftp,
                &local_path,
                &remote_path,
                transfer,
                transferred_clone,
                Some(total_bytes),
            )
            .await
        });
    }

    use futures::StreamExt as _;
    let mut stream = futures::stream::iter(futures).buffer_unordered(4);
    while let Some(res) = stream.next().await {
        res?;
    }

    let _ = events.send(BackendEvent::TransferProgress {
        tab_id: tab_id.to_string(),
        id: id.to_string(),
        transferred: total_bytes,
        total: Some(total_bytes),
        state: crate::terminal::TransferState::Completed,
    });

    let summary = if file_count == 1 && folder_count == 0 {
        t!("uploaded_file").to_string()
    } else if file_count == 0 && folder_count == 1 {
        t!("uploaded_folder").to_string()
    } else if file_count > 0 && folder_count == 0 {
        t!("uploaded_n_files", files = file_count).to_string()
    } else if file_count == 0 && folder_count > 0 {
        t!("uploaded_n_folders", folders = folder_count).to_string()
    } else {
        t!(
            "uploaded_files_and_folders",
            files = file_count,
            folders = folder_count
        )
        .to_string()
    };
    Ok(summary)
}

async fn upload_file_impl(
    sftp: &SftpSession,
    local_file: &Path,
    remote_path: &str,
    transfer: TransferContext<'_>,
    transferred: Arc<AtomicU64>,
    total: Option<u64>,
) -> Result<()> {
    let mut local = tokio::fs::File::open(local_file)
        .await
        .with_context(|| format!("open local {}", local_file.display()))?;
    let mut remote = sftp
        .create(remote_path)
        .await
        .with_context(|| format!("create remote {remote_path}"))?;

    let mut buffer = vec![0u8; 128 * 1024];
    loop {
        let cur = transferred.load(Ordering::Relaxed);
        transfer
            .flag
            .yield_if_paused(transfer.events, transfer.tab_id, transfer.id, cur, total)
            .await?;
        let read = local.read(&mut buffer).await.context("read local file")?;
        if read == 0 {
            break;
        }
        remote
            .write_all(&buffer[..read])
            .await
            .with_context(|| format!("write remote {remote_path}"))?;

        let new_cur = transferred.fetch_add(read as u64, Ordering::Relaxed) + read as u64;
        let _ = transfer.events.send(BackendEvent::TransferProgress {
            tab_id: transfer.tab_id.to_string(),
            id: transfer.id.to_string(),
            transferred: new_cur,
            total,
            state: crate::terminal::TransferState::Running,
        });
    }
    remote.flush().await.context("flush remote file")?;
    Ok(())
}

async fn create_remote_dir_all(sftp: &SftpSession, remote_dir: &str) -> Result<()> {
    if remote_dir.is_empty() || remote_dir == "/" {
        return Ok(());
    }

    let mut current = String::from("/");
    for segment in remote_dir.split('/').filter(|segment| !segment.is_empty()) {
        current = join_remote(&current, segment);
        let _ = sftp.create_dir(&current).await;
    }
    Ok(())
}

async fn create_remote_archive(
    handle: &russh::client::Handle<SftpClientHandler>,
    remote_dir: &str,
    remote_archive: &str,
) -> Result<()> {
    let remote_dir = remote_dir.trim_end_matches('/');
    let parent = remote_parent(remote_dir);
    let name = base_name(remote_dir);
    let command = format!(
        "tar -C {} -czf {} {}",
        shell_quote(&parent),
        shell_quote(remote_archive),
        shell_quote(&name),
    );
    exec_remote_command(handle, &command)
        .await
        .with_context(|| format!("archive remote directory {remote_dir}"))?;
    Ok(())
}

async fn remove_remote_path(
    handle: &russh::client::Handle<SftpClientHandler>,
    remote_path: &str,
) -> Result<()> {
    let command = format!("rm -f {}", shell_quote(remote_path));
    exec_remote_command(handle, &command)
        .await
        .with_context(|| format!("remove remote temporary file {remote_path}"))?;
    Ok(())
}

async fn exec_remote_command(
    handle: &russh::client::Handle<SftpClientHandler>,
    command: &str,
) -> Result<()> {
    let mut channel = handle
        .channel_open_session()
        .await
        .context("open remote exec session")?;
    channel
        .exec(true, command)
        .await
        .with_context(|| format!("exec remote command: {command}"))?;

    let mut stderr = Vec::new();
    let mut stdout = Vec::new();
    let mut exit_status = None;

    // Add timeout to prevent indefinite blocking (300 seconds = 5 minutes)
    let timeout = tokio::time::Duration::from_secs(300);
    let result = tokio::time::timeout(timeout, async {
        loop {
            // Yield to allow cancellation
            tokio::task::yield_now().await;

            if let Some(msg) = channel.wait().await {
                match msg {
                    russh::ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
                    russh::ChannelMsg::ExtendedData { data, .. } => stderr.extend_from_slice(&data),
                    russh::ChannelMsg::ExitStatus { exit_status: code } => exit_status = Some(code),
                    russh::ChannelMsg::Close => break,
                    _ => {}
                }
            } else {
                break;
            }
        }
    })
    .await;

    if result.is_err() {
        return Err(anyhow!("remote command timeout: {command}"));
    }

    match exit_status.unwrap_or(0) {
        0 => Ok(()),
        code => {
            let stderr = String::from_utf8_lossy(&stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&stdout).trim().to_string();
            Err(anyhow!(
                "remote command exited with {code}: {}",
                if !stderr.is_empty() { stderr } else { stdout }
            ))
        }
    }
}

fn remote_parent(path: &str) -> String {
    if path == "/" {
        "/".to_string()
    } else {
        path.rsplit_once('/')
            .map(|(parent, _)| {
                if parent.is_empty() {
                    "/".to_string()
                } else {
                    parent.to_string()
                }
            })
            .unwrap_or_else(|| "/".to_string())
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[allow(dead_code)]
async fn maybe_extract_archive(path: &Path) -> Result<Option<PathBuf>> {
    let Some(file_name) = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_string())
    else {
        return Ok(None);
    };
    let is_archive = [".zip", ".tar", ".tar.gz", ".tgz"]
        .iter()
        .any(|suffix| file_name.ends_with(suffix));
    if !is_archive {
        return Ok(None);
    }

    let extract_root = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(strip_archive_suffix(&file_name));
    let archive_path = path.to_path_buf();
    let target_dir = extract_root.clone();

    tokio::task::spawn_blocking(move || -> Result<()> {
        fs::create_dir_all(&target_dir)
            .with_context(|| format!("create {}", target_dir.display()))?;

        if file_name.ends_with(".zip") {
            let file = fs::File::open(&archive_path)
                .with_context(|| format!("open {}", archive_path.display()))?;
            let mut zip = ZipArchive::new(file).context("read zip archive")?;
            for index in 0..zip.len() {
                let mut entry = zip.by_index(index).context("read zip entry")?;
                let Some(name) = entry.enclosed_name().map(|name| name.to_path_buf()) else {
                    continue;
                };
                let output = target_dir.join(name);
                if entry.is_dir() {
                    fs::create_dir_all(&output)?;
                } else {
                    if let Some(parent) = output.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    let mut output_file = fs::File::create(&output)?;
                    std::io::copy(&mut entry, &mut output_file)?;
                }
            }
        } else if file_name.ends_with(".tar.gz") || file_name.ends_with(".tgz") {
            let file = fs::File::open(&archive_path)
                .with_context(|| format!("open {}", archive_path.display()))?;
            let decoder = GzDecoder::new(file);
            let mut archive = tar::Archive::new(decoder);
            archive
                .unpack(&target_dir)
                .context("unpack tar.gz archive")?;
        } else if file_name.ends_with(".tar") {
            let file = fs::File::open(&archive_path)
                .with_context(|| format!("open {}", archive_path.display()))?;
            let mut archive = tar::Archive::new(file);
            archive.unpack(&target_dir).context("unpack tar archive")?;
        }

        Ok(())
    })
    .await
    .context("extract archive task join failure")??;

    Ok(Some(extract_root))
}

async fn extract_archive_to(path: &Path, target_dir: &Path) -> Result<()> {
    let Some(file_name) = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_string())
    else {
        return Ok(());
    };
    let archive_path = path.to_path_buf();
    let target_dir = target_dir.to_path_buf();

    tokio::task::spawn_blocking(move || -> Result<()> {
        fs::create_dir_all(&target_dir)
            .with_context(|| format!("create {}", target_dir.display()))?;

        if file_name.ends_with(".zip") {
            let file = fs::File::open(&archive_path)
                .with_context(|| format!("open {}", archive_path.display()))?;
            let mut zip = ZipArchive::new(file).context("read zip archive")?;
            for index in 0..zip.len() {
                let mut entry = zip.by_index(index).context("read zip entry")?;
                let Some(name) = entry.enclosed_name().map(|name| name.to_path_buf()) else {
                    continue;
                };
                let output = target_dir.join(name);
                if entry.is_dir() {
                    fs::create_dir_all(&output)?;
                } else {
                    if let Some(parent) = output.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    let mut output_file = fs::File::create(&output)?;
                    std::io::copy(&mut entry, &mut output_file)?;
                }
            }
        } else if file_name.ends_with(".tar.gz") || file_name.ends_with(".tgz") {
            let file = fs::File::open(&archive_path)
                .with_context(|| format!("open {}", archive_path.display()))?;
            let decoder = GzDecoder::new(file);
            let mut archive = tar::Archive::new(decoder);
            archive
                .unpack(&target_dir)
                .context("unpack tar.gz archive")?;
        } else if file_name.ends_with(".tar") {
            let file = fs::File::open(&archive_path)
                .with_context(|| format!("open {}", archive_path.display()))?;
            let mut archive = tar::Archive::new(file);
            archive.unpack(&target_dir).context("unpack tar archive")?;
        }

        Ok(())
    })
    .await
    .context("extract archive task join failure")??;

    Ok(())
}

#[derive(Clone)]
struct SftpClientHandler;

#[async_trait]
impl Handler for SftpClientHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

#[cfg(test)]
mod path_tests {
    use super::{normalize_remote_path, remote_path_ancestors};

    #[test]
    fn normalizes_remote_paths_without_platform_separators() {
        assert_eq!(
            normalize_remote_path("~", "/tmp", "/home/demo"),
            "/home/demo"
        );
        assert_eq!(
            normalize_remote_path("../logs", "/srv/app/current", "/home/demo"),
            "/srv/app/logs"
        );
        assert_eq!(
            normalize_remote_path("/var//log/", "/", "/home/demo"),
            "/var/log"
        );
    }

    #[test]
    fn builds_remote_path_ancestors_from_root() {
        assert_eq!(
            remote_path_ancestors("/home/demo/projects"),
            vec!["/", "/home", "/home/demo", "/home/demo/projects"]
        );
    }
}

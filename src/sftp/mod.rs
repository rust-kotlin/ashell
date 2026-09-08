mod archive;
mod events;
pub mod ops;
#[cfg(test)]
mod security_tests;
mod transfer;

use self::events::{SftpEventSender, SftpOwner};
use self::transfer::{TransferQueue, TransferRegistry, TransferStateFlag};
use crate::backend::connection::{ConnectionControl, SshConnection, connect_with_timeout};

const MAX_CONCURRENT_TRANSFERS: usize = 3;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, TimeZone, Utc};
use directories::BaseDirs;
use russh::{
    client::{self, Handler},
    keys::{PrivateKey, decode_secret_key, load_secret_key},
};
use russh_sftp::{
    client::SftpSession,
    protocol::{FileAttributes, OpenFlags},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{
        mpsc::{self, UnboundedReceiver, UnboundedSender},
        oneshot,
    },
    task::JoinSet,
};
use uuid::Uuid;
use walkdir::WalkDir;

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
}

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy)]
struct TransferContext<'a> {
    flag: &'a TransferStateFlag,
    events: &'a std::sync::mpsc::Sender<BackendEvent>,
    id: &'a str,
}

#[derive(Clone)]
pub struct SftpHandle {
    pub commands: UnboundedSender<SftpCommand>,
    connection_id: String,
    owner: Arc<SftpOwner>,
    transfers: TransferRegistry,
}

impl SftpHandle {
    pub(crate) fn connection_id(&self) -> &str {
        &self.connection_id
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.owner.is_cancelled() || self.commands.is_closed()
    }

    pub(crate) fn same_connection(&self, other: &Self) -> bool {
        self.commands.same_channel(&other.commands)
    }

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
        self.owner.cancel();
    }

    pub fn has_transfer(&self, id: &str) -> bool {
        self.transfers.flag(id).is_some()
    }

    pub fn pause_transfer(&self, id: &str) {
        if let Some(flag) = self.transfers.flag(id) {
            flag.pause();
        }
    }

    pub fn resume_transfer(&self, id: &str) {
        if let Some(flag) = self.transfers.flag(id) {
            flag.resume();
        }
    }

    pub fn cancel_transfer(&self, id: &str) {
        if let Some(flag) = self.transfers.flag(id) {
            flag.cancel();
        }
    }
}

pub fn spawn_sftp(
    runtime: &tokio::runtime::Handle,
    tab_id: String,
    session: Session,
    events: std::sync::mpsc::Sender<BackendEvent>,
    attempt: crate::terminal::BackendAttempt,
) -> SftpHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let cmd_tx_clone = cmd_tx.clone();
    let control = ConnectionControl::new();
    let events = SftpEventSender::new(events);
    let owner = Arc::new(SftpOwner::new(control.guard(), events.clone()));
    let transfers = TransferRegistry::default();
    let worker_transfers = transfers.clone();
    runtime.spawn(async move {
        let _connection_guard = control.guard();
        let result = tokio::select! {
            biased;
            _ = control.cancelled() => Ok(()),
            result = run_sftp(tab_id.clone(), session, cmd_rx, cmd_tx_clone, events.clone(), control.clone(), worker_transfers, attempt) => result,
        };
        if let Err(err) = result {
            let _ = events.send(BackendEvent::SftpStatus {
                tab_id,
                text: format!("sftp error: {err:#}"),
            });
        }
    });
    SftpHandle {
        commands: cmd_tx,
        connection_id: Uuid::new_v4().to_string(),
        owner,
        transfers,
    }
}

async fn run_sftp(
    tab_id: String,
    session: Session,
    commands: UnboundedReceiver<SftpCommand>,
    commands_tx: UnboundedSender<SftpCommand>,
    events: SftpEventSender,
    control: ConnectionControl,
    active_transfers: TransferRegistry,
    attempt: crate::terminal::BackendAttempt,
) -> Result<()> {
    let _ = events.send(BackendEvent::SftpStatus {
        tab_id: tab_id.clone(),
        text: t!("sftp_connecting").to_string(),
    });

    let (handle, sftp, home) = connect_with_timeout(async {
        let handle =
            connect_and_authenticate(&tab_id, &session, control, events.clone(), attempt).await?;
        let sftp = open_sftp_session(&handle).await?;
        let home = sftp
            .canonicalize(".")
            .await
            .unwrap_or_else(|_| "/".to_string());
        Ok((handle, sftp, home))
    })
    .await?;

    let _ = events.send(BackendEvent::SftpHome {
        tab_id: tab_id.clone(),
        home: home.clone(),
    });

    emit_entries(&events, &tab_id, &sftp, &home).await?;

    let result = tokio::select! {
        biased;
        _ = handle.closed() => Err(anyhow!("SFTP connection lost")),
        result = run_sftp_commands(tab_id, handle.clone(), sftp, home, commands, commands_tx, events, active_transfers) => result,
    };
    handle.close();
    result
}

async fn run_sftp_commands(
    tab_id: String,
    handle: Arc<SshConnection<SftpClientHandler>>,
    sftp: SftpSession,
    home: String,
    mut commands: UnboundedReceiver<SftpCommand>,
    commands_tx: UnboundedSender<SftpCommand>,
    events: SftpEventSender,
    active_transfers: TransferRegistry,
) -> Result<()> {
    let mut tasks = JoinSet::new();
    let slots = TransferQueue::new(MAX_CONCURRENT_TRANSFERS);
    loop {
        let command = tokio::select! {
            _ = tasks.join_next(), if !tasks.is_empty() => continue,
            command = commands.recv() => command,
        };
        let Some(command) = command else {
            break;
        };
        match command {
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
                let mut completion = active_transfers.register(
                    id.clone(),
                    flag.clone(),
                    events.transfer_events().clone(),
                );

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

                let slots = slots.clone();
                let cancellation = flag.clone();
                tasks.spawn(async move {
                    let result = async {
                        let _permit = slots.acquire(&cancellation).await?;
                        flag.yield_if_paused(events_clone.transfer_events(), &id, 0, None)
                            .await?;
                        // Finish the bounded channel-open handshake before handling
                        // cancellation, so another transfer's connection stays usable.
                        let sftp_session = open_sftp_session(&handle_clone).await?;
                        let transfer = async {
                            let _ = events_clone.send(BackendEvent::TransferProgress {
                                id: id.clone(),
                                transferred: 0,
                                total: None,
                                state: crate::terminal::TransferState::Running,
                            });
                            let _ = events_clone.send(BackendEvent::SftpStatus {
                                tab_id: tab_id_clone.clone(),
                                text: t!("downloading_file", base = base_name(&remote)).to_string(),
                            });
                            let transfer = TransferContext {
                                flag: &flag,
                                events: events_clone.transfer_events(),
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
                        };
                        transfer.await
                    }
                    .await;

                    match result {
                        Ok(summary) => {
                            let _ = events_clone.send(BackendEvent::TransferProgress {
                                id: id.clone(),
                                transferred: 0,
                                total: None,
                                state: crate::terminal::TransferState::Completed,
                            });
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
                                tab_id: tab_id_clone,
                                text: if is_cancelled {
                                    "Transmission cancelled".to_string()
                                } else {
                                    t!("download_failed", err = err_msg.clone()).to_string()
                                },
                            });
                            let _ = events_clone.send(BackendEvent::TransferProgress {
                                id: id.clone(),
                                transferred: 0,
                                total: None,
                                state,
                            });
                        }
                    }
                    completion.finish();
                });
            }
            SftpCommand::UploadPaths { locals, remote_dir } => {
                let id = uuid::Uuid::new_v4().to_string();
                let flag = TransferStateFlag::new();
                let mut completion = active_transfers.register(
                    id.clone(),
                    flag.clone(),
                    events.transfer_events().clone(),
                );

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

                let slots = slots.clone();
                let cancellation = flag.clone();
                tasks.spawn(async move {
                    let result = async {
                        let _permit = slots.acquire(&cancellation).await?;
                        flag.yield_if_paused(events_clone.transfer_events(), &id, 0, None)
                            .await?;
                        // Finish the bounded channel-open handshake before handling
                        // cancellation, so another transfer's connection stays usable.
                        let sftp_session = open_sftp_session(&handle_clone).await?;
                        let transfer = async {
                            let _ = events_clone.send(BackendEvent::TransferProgress {
                                id: id.clone(),
                                transferred: 0,
                                total: None,
                                state: crate::terminal::TransferState::Running,
                            });
                            let _ = events_clone.send(BackendEvent::SftpStatus {
                                tab_id: tab_id_clone.clone(),
                                text: t!("uploading").to_string(),
                            });
                            upload_paths_impl(
                                &sftp_session,
                                &locals,
                                &remote_dir,
                                flag,
                                events_clone.transfer_events(),
                                &id,
                            )
                            .await
                        };
                        tokio::select! {
                            biased;
                            _ = cancellation.cancelled() => Err(anyhow!("transfer cancelled")),
                            result = transfer => result,
                        }
                    }
                    .await;

                    match result {
                        Ok(summary) => {
                            let _ = events_clone.send(BackendEvent::TransferProgress {
                                id: id.clone(),
                                transferred: 0,
                                total: None,
                                state: crate::terminal::TransferState::Completed,
                            });
                            let _ = events_clone.send(BackendEvent::SftpStatus {
                                tab_id: tab_id_clone,
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
                                tab_id: tab_id_clone,
                                text: if is_cancelled {
                                    "Transmission cancelled".to_string()
                                } else {
                                    t!("upload_failed", err = err_msg.clone()).to_string()
                                },
                            });
                            let _ = events_clone.send(BackendEvent::TransferProgress {
                                id: id.clone(),
                                transferred: 0,
                                total: None,
                                state,
                            });
                        }
                    }
                    completion.finish();
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
                let result = write_text_file_impl(&handle, &sftp, &remote_path, &content)
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

    Ok(())
}

async fn open_sftp_session(handle: &SshConnection<SftpClientHandler>) -> Result<SftpSession> {
    connect_with_timeout(async {
        let channel = handle
            .open_session_channel()
            .await
            .context("open sftp channel")?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .context("request sftp subsystem")?;
        SftpSession::new(channel.into_stream())
            .await
            .context("sftp handshake")
    })
    .await
}

use std::future::Future;
use std::pin::Pin;

fn recursive_delete<'a>(
    sftp: &'a SftpSession,
    path: String,
) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
    recursive_delete_checked(sftp, path, Vec::new())
}

/// Recheck the traversed directories at request boundaries. SFTP v3 cannot
/// express unlinkat/no-follow, so this fails closed when a replacement is seen.
fn recursive_delete_checked<'a>(
    sftp: &'a SftpSession,
    path: String,
    ancestors: Vec<String>,
) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(async move {
        let path = deletion_path(&path)?;
        check_delete_ancestors(sftp, &ancestors).await?;
        let metadata = sftp
            .symlink_metadata(&path)
            .await
            .with_context(|| format!("inspect deletion target {path}"))?;
        let kind = metadata
            .permissions
            .context("server omitted deletion target type")?
            & 0o170_000;
        if kind == 0 {
            return Err(anyhow!("server omitted deletion target type"));
        }
        if kind == 0o040_000 {
            let mut children_ancestors = ancestors.clone();
            children_ancestors.push(path.clone());
            check_delete_ancestors(sftp, &children_ancestors).await?;
            let entries = sftp
                .read_dir(&path)
                .await
                .with_context(|| format!("read deletion directory {path}"))?;
            check_delete_ancestors(sftp, &children_ancestors).await?;
            for entry in entries {
                let name = entry.file_name();
                if name == "." || name == ".." {
                    continue;
                }
                if name.contains(['/', '\\', '\0']) {
                    return Err(anyhow!("server returned an invalid directory entry"));
                }
                let child_path = crate::sftp::join_remote(&path, &name);
                recursive_delete_checked(sftp, child_path, children_ancestors.clone()).await?;
            }
            check_delete_ancestors(sftp, &children_ancestors).await?;
            sftp.remove_dir(&path)
                .await
                .with_context(|| format!("Failed to delete dir {path}"))?;
        } else {
            check_delete_ancestors(sftp, &ancestors).await?;
            sftp.remove_file(&path)
                .await
                .with_context(|| format!("Failed to delete {path}"))?;
        }
        Ok(())
    })
}

async fn check_delete_ancestors(sftp: &SftpSession, ancestors: &[String]) -> Result<()> {
    for ancestor in ancestors {
        let metadata = sftp
            .symlink_metadata(ancestor)
            .await
            .with_context(|| format!("recheck deletion directory {ancestor}"))?;
        if metadata
            .permissions
            .is_none_or(|mode| mode & 0o170_000 != 0o040_000)
        {
            return Err(anyhow!(
                "deletion directory changed during traversal: {ancestor}"
            ));
        }
    }
    Ok(())
}

/// A trailing separator must not turn an LSTAT of a link into a lookup of its target.
fn deletion_path(path: &str) -> Result<String> {
    if path.contains(['\\', '\0']) || path.split('/').any(|part| matches!(part, "." | "..")) {
        return Err(anyhow!("ambiguous deletion path"));
    }
    let path = path.trim_end_matches('/');
    if path.is_empty() {
        return Err(anyhow!("refusing to delete the filesystem root"));
    }
    Ok(path.to_string())
}

async fn emit_entries(
    events: &SftpEventSender,
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
    tab_id: &str,
    session: &Session,
    control: ConnectionControl,
    events: SftpEventSender,
    attempt: crate::terminal::BackendAttempt,
) -> Result<Arc<SshConnection<SftpClientHandler>>> {
    let config = Arc::new(crate::session::config::ssh_client_config());
    let addr = format!("{}:{}", session.host, session.port);
    let stream = crate::session::config::connect_proxy(session).await?;
    let stream = control.attach(stream)?;
    let mut handle = client::connect_stream(
        config,
        stream,
        SftpClientHandler {
            tab_id: tab_id.to_string(),
            host: session.host.clone(),
            port: session.port,
            events,
            attempt,
        },
    )
    .await
    .with_context(|| format!("connect {addr} failed"))?;

    let authed = match session.auth {
        AuthMethod::Password => handle
            .authenticate_password(&session.user, &session.password)
            .await
            .context("password authentication failed")?
            .success(),
        AuthMethod::Key => {
            let has_explicit_key = session_has_explicit_key(session);
            if has_explicit_key {
                let keypair = load_session_private_key(session)?;
                let keys = private_keys_with_algs(keypair);
                let mut success = false;
                for key in keys {
                    match handle.authenticate_publickey(&session.user, key).await {
                        Ok(result) if result.success() => {
                            success = true;
                            break;
                        }
                        Ok(_) => {
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
                let keys = private_keys_with_algs(keypair);
                let mut success = false;
                for key in keys {
                    match handle.authenticate_publickey(&session.user, key).await {
                        Ok(result) if result.success() => {
                            success = true;
                            break;
                        }
                        Ok(_) => {
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

    Ok(Arc::new(SshConnection::new(handle, control)))
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

/// Write a complete private temporary file before publishing an atomic replacement.
async fn write_text_file_impl(
    handle: &SshConnection<SftpClientHandler>,
    sftp: &SftpSession,
    path: &str,
    content: &[u8],
) -> Result<()> {
    write_remote_file_with_commit(sftp, path, content, |temporary, target| async move {
        atomic_replace_remote_file(handle, sftp, &temporary, &target).await
    })
    .await
}

async fn write_remote_file_with_commit<F, Fut>(
    sftp: &SftpSession,
    path: &str,
    content: &[u8],
    commit: F,
) -> Result<()>
where
    F: FnOnce(String, String) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    if content.len() > MAX_INLINE_EDIT_BYTES {
        return Err(anyhow!("file exceeds the 2 MB in-app editing limit"));
    }
    let original = sftp
        .symlink_metadata(path)
        .await
        .context("inspect remote file before saving")?;
    let kind = original
        .permissions
        .context("server omitted remote file type")?
        & 0o170_000;
    // Preserve a file symlink itself: publish to its resolved target instead.
    let path = if kind == 0o120_000 {
        sftp.canonicalize(path)
            .await
            .context("resolve remote file link")?
    } else {
        path.to_string()
    };
    let metadata = sftp
        .metadata(&path)
        .await
        .context("inspect remote save target")?;
    if metadata
        .permissions
        .is_none_or(|permissions| permissions & 0o170_000 != 0o100_000)
    {
        return Err(anyhow!("remote save target is not a regular file"));
    }
    let temporary_path = format!("{path}.ashell-{}.tmp", Uuid::new_v4());
    let mut temporary_created = false;
    let write_result = async {
        let mut remote_file = sftp
            .open_with_flags_and_attributes(
                temporary_path.as_str(),
                OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
                FileAttributes {
                    permissions: Some(0o600),
                    ..FileAttributes::default()
                },
            )
            .await
            .context("create exclusive remote temporary file")?;
        temporary_created = true;
        remote_file
            .write_all(content)
            .await
            .context("write remote temporary file")?;
        remote_file
            .flush()
            .await
            .context("flush remote temporary file")?;
        remote_file
            .sync_all()
            .await
            .context("sync remote temporary file")?;
        remote_file
            .close()
            .await
            .context("close remote temporary file")?;
        sftp.set_metadata(
            &temporary_path,
            FileAttributes {
                permissions: metadata.permissions,
                uid: metadata.uid,
                gid: metadata.gid,
                ..FileAttributes::default()
            },
        )
        .await
        .context("preserve remote file ownership and permissions")?;
        commit(temporary_path.clone(), path.clone()).await
    }
    .await;
    if let Err(error) = write_result {
        if temporary_created {
            let _ = sftp.remove_file(&temporary_path).await;
        }
        return Err(error)
            .context("remote save could not be confirmed; reload the file to check its contents");
    }
    Ok(())
}

async fn atomic_replace_remote_file(
    handle: &SshConnection<SftpClientHandler>,
    sftp: &SftpSession,
    temporary: &str,
    target: &str,
) -> Result<()> {
    connect_with_timeout(async {
        let channel = handle.open_session_channel().await?;
        channel.request_subsystem(true, "sftp").await?;
        let raw = russh_sftp::client::RawSftpSession::new(channel.into_stream());
        let version = raw
            .init()
            .await
            .context("initialize atomic SFTP replacement")?;
        if version
            .extensions
            .get("posix-rename@openssh.com")
            .is_some_and(|version| version == "1")
        {
            let data = russh_sftp::ser::to_bytes(&(temporary, target))?.to_vec();
            match raw.extended("posix-rename@openssh.com", data).await? {
                russh_sftp::protocol::Packet::Status(status)
                    if status.status_code == russh_sftp::protocol::StatusCode::Ok =>
                {
                    Ok(())
                }
                russh_sftp::protocol::Packet::Status(status) => Err(anyhow!(
                    "atomic replacement failed: {}",
                    status.error_message
                )),
                _ => Err(anyhow!("unexpected atomic replacement response")),
            }
        } else {
            // Standard rename may succeed on some servers. Failure must never
            // turn into deletion or truncation of the existing destination.
            sftp.rename(temporary, target)
                .await
                .context("server does not support safe replacement of this file")
        }
    })
    .await
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
    handle: &SshConnection<SftpClientHandler>,
    sftp: &SftpSession,
    remote: &str,
    local_dir: &Path,
    transfer: TransferContext<'_>,
) -> Result<String> {
    tokio::fs::create_dir_all(local_dir)
        .await
        .with_context(|| format!("create {}", local_dir.display()))?;

    // Check for cancellation after initial setup
    if transfer.flag.is_cancelled() {
        return Err(anyhow::anyhow!("transfer cancelled"));
    }

    let metadata = transfer
        .flag
        .run(async {
            sftp.metadata(remote)
                .await
                .with_context(|| format!("metadata {remote}"))
        })
        .await?;
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
            let _ = maybe_extract_archive(&local_path, transfer.flag).await;
        }
    }
    Ok(())
}

async fn download_remote_directory_archive(
    handle: &SshConnection<SftpClientHandler>,
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
    let local_extract_root = local_archive
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(base_name(remote_dir));
    transfer.flag.run(async { Ok(()) }).await?;

    let archive_download = async {
        transfer
            .flag
            .run(create_remote_archive(handle, remote_dir, &remote_archive))
            .await?;
        download_file_impl(sftp, &remote_archive, local_archive, transfer).await?;
        // Wait for the cooperative blocking worker to stop. Dropping its handle
        // would let the task outlive both its queue permit and cleanup.
        extract_archive_to(
            local_archive,
            local_archive.parent().unwrap_or_else(|| Path::new(".")),
            transfer.flag,
        )
        .await?;
        tokio::fs::remove_file(local_archive)
            .await
            .with_context(|| format!("remove {}", local_archive.display()))?;
        Ok::<PathBuf, anyhow::Error>(local_extract_root)
    }
    .await;

    // This runs after ordinary cancellation too, using the existing SFTP channel.
    // A broken transport may prevent cleanup; report that instead of reconnecting.
    let cleanup = sftp.remove_file(&remote_archive).await;
    if let Err(error) = cleanup {
        if !matches!(&error, russh_sftp::client::error::Error::Status(status)
            if status.status_code == russh_sftp::protocol::StatusCode::NoSuchFile)
        {
            tracing::warn!("failed to clean remote archive {remote_archive}: {error}");
        }
    }
    archive_download
}

async fn download_file_impl(
    sftp: &SftpSession,
    remote: &str,
    local: &Path,
    transfer: TransferContext<'_>,
) -> Result<()> {
    transfer
        .flag
        .run(async {
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
                    .yield_if_paused(transfer.events, transfer.id, transferred, total)
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
                    id: transfer.id.to_string(),
                    transferred,
                    total,
                    state: crate::terminal::TransferState::Running,
                });
            }
            local_file.flush().await.context("flush local file")?;

            Ok(())
        })
        .await
}

async fn upload_paths_impl(
    sftp: &SftpSession,
    locals: &[String],
    remote_dir: &str,
    flag: TransferStateFlag,
    events: &std::sync::mpsc::Sender<BackendEvent>,
    id: &str,
) -> Result<String> {
    // Check for cancellation before starting
    if flag.is_cancelled() {
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
        let metadata = tokio::fs::metadata(&p)
            .await
            .with_context(|| format!("inspect local upload source {}", p.display()))?;
        if metadata.is_dir() {
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

                let metadata = tokio::fs::metadata(path)
                    .await
                    .with_context(|| format!("inspect local upload source {}", path.display()))?;
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

                if metadata.is_dir() {
                    dirs_to_create.push(remote_path);
                } else if metadata.is_file() {
                    total_bytes += metadata.len();
                    files_to_upload.push((path.to_path_buf(), remote_path));
                } else {
                    return Err(anyhow!(
                        "upload source is not a regular file: {}",
                        path.display()
                    ));
                }
            }
        } else if metadata.is_file() {
            total_bytes += metadata.len();
            let file_name = p.file_name().and_then(|n| n.to_str()).unwrap_or("file");
            files_to_upload.push((p.clone(), join_remote(remote_dir, file_name)));
            file_count += 1;
        } else {
            return Err(anyhow!(
                "upload source is not a regular file: {}",
                p.display()
            ));
        }
    }

    // Check for cancellation before creating directories
    if flag.is_cancelled() {
        return Err(anyhow::anyhow!("transfer cancelled"));
    }

    // Create directories sequentially first
    for dir in dirs_to_create {
        // Check for cancellation between each directory creation
        if flag.is_cancelled() {
            return Err(anyhow::anyhow!("transfer cancelled"));
        }
        create_remote_dir_all(sftp, &dir).await?;
    }

    let transferred = Arc::new(AtomicU64::new(0));
    let mut futures = Vec::new();

    for (local_path, remote_path) in files_to_upload {
        let flag_clone = flag.clone();
        let events_clone = events.clone();
        let id_clone = id.to_string();
        let transferred_clone = Arc::clone(&transferred);

        futures.push(async move {
            let transfer = TransferContext {
                flag: &flag_clone,
                events: &events_clone,
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
            .yield_if_paused(transfer.events, transfer.id, cur, total)
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
            id: transfer.id.to_string(),
            transferred: new_cur,
            total,
            state: crate::terminal::TransferState::Running,
        });
    }
    remote.flush().await.context("flush remote file")?;
    // CLOSE can report delayed write failures (for example, quota exhaustion).
    // Dropping russh-sftp's file does not await that acknowledgement.
    remote.close().await.context("close remote file")?;
    Ok(())
}

async fn create_remote_dir_all(sftp: &SftpSession, remote_dir: &str) -> Result<()> {
    if remote_dir.is_empty() || remote_dir == "/" {
        return Ok(());
    }

    let mut current = String::from("/");
    for segment in remote_dir.split('/').filter(|segment| !segment.is_empty()) {
        current = join_remote(&current, segment);
        if let Err(error) = sftp.create_dir(&current).await {
            // Existing directories are expected. Permission errors and file
            // collisions must not turn an empty-folder upload into a success.
            let is_directory = sftp.metadata(&current).await.is_ok_and(|metadata| {
                metadata
                    .permissions
                    .is_some_and(|mode| mode & 0o170_000 == 0o040_000)
            });
            if !is_directory {
                return Err(error).with_context(|| format!("create remote directory {current}"));
            }
        }
    }
    Ok(())
}

async fn create_remote_archive(
    handle: &SshConnection<SftpClientHandler>,
    remote_dir: &str,
    remote_archive: &str,
) -> Result<()> {
    let command = remote_archive_command(remote_dir, remote_archive);
    exec_remote_command(handle, &command)
        .await
        .with_context(|| format!("archive remote directory {remote_dir}"))?;
    Ok(())
}

/// Quote shell syntax and stop tar option parsing before the selected filename.
fn remote_archive_command(remote_dir: &str, remote_archive: &str) -> String {
    let remote_dir = remote_dir.trim_end_matches('/');
    let parent = remote_parent(remote_dir);
    let name = base_name(remote_dir);
    format!(
        "umask 077; trap {} HUP INT TERM; tar -C {} -czf {} -- {}; status=$?; if [ \"$status\" -ne 0 ]; then {}; fi; exit \"$status\"",
        shell_quote(&format!("rm -f -- {}; exit 1", shell_quote(remote_archive))),
        shell_quote(&parent),
        shell_quote(remote_archive),
        shell_quote(&name),
        format!("rm -f -- {}", shell_quote(remote_archive)),
    )
}

async fn exec_remote_command(
    handle: &SshConnection<SftpClientHandler>,
    command: &str,
) -> Result<()> {
    let mut channel = handle
        .open_session_channel()
        .await
        .context("open remote exec session")?;

    let mut stderr = Vec::new();
    let mut stdout = Vec::new();
    let mut exit_status = None;

    // Add timeout to prevent indefinite blocking (300 seconds = 5 minutes)
    let timeout = tokio::time::Duration::from_secs(300);
    let result = tokio::time::timeout(timeout, async {
        channel
            .exec(true, command)
            .await
            .with_context(|| format!("exec remote command: {command}"))?;

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
        Ok::<(), anyhow::Error>(())
    })
    .await;

    result.with_context(|| format!("remote command timeout: {command}"))??;

    match exit_status.context("remote command closed without an exit status")? {
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
async fn maybe_extract_archive(path: &Path, flag: &TransferStateFlag) -> Result<Option<PathBuf>> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    if ![".zip", ".tar", ".tar.gz", ".tgz"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
    {
        return Ok(None);
    }
    let root = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(strip_archive_suffix(name));
    archive::extract(path, &root, flag.clone()).await?;
    Ok(Some(root))
}

async fn extract_archive_to(
    path: &Path,
    target_dir: &Path,
    flag: &TransferStateFlag,
) -> Result<()> {
    archive::extract(path, target_dir, flag.clone()).await
}

#[derive(Clone)]
struct SftpClientHandler {
    tab_id: String,
    host: String,
    port: u16,
    events: SftpEventSender,
    attempt: crate::terminal::BackendAttempt,
}

impl Handler for SftpClientHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        crate::session::host_keys::verify(
            &self.tab_id,
            &self.host,
            self.port,
            server_public_key,
            |mut event| {
                if let BackendEvent::HostKeyVerification(request) = &mut event {
                    request.attempt = Some(self.attempt.clone());
                    request.sftp_attempt = Some(self.events.attempt());
                }
                let _ = self.events.send(event);
            },
        )
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

    #[cfg(unix)]
    #[test]
    fn archive_commands_pass_option_like_names_and_shell_characters_as_literal_paths() {
        // Replace tar with an argument recorder; no archive or remote files are created.
        for name in [
            "--version",
            "--checkpoint-action=exec=echo",
            "a b'c;$(false)",
        ] {
            let command =
                super::remote_archive_command(&format!("/data/{name}"), "/tmp/archive.tar.gz");
            let script = format!("tar() {{ printf '%s\\n' \"$@\"; }};\n{command}");
            let result = std::process::Command::new("sh")
                .arg("-c")
                .arg(script)
                .output()
                .unwrap();
            assert!(result.status.success());
            let output = String::from_utf8(result.stdout).unwrap();
            assert_eq!(
                output.lines().collect::<Vec<_>>(),
                vec!["-C", "/data", "-czf", "/tmp/archive.tar.gz", "--", name]
            );
        }
    }
}

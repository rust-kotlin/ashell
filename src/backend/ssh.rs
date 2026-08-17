use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use base64::Engine as _;
use directories::BaseDirs;
use russh::{
    ChannelMsg, Disconnect,
    client::{self, Handler},
    keys::{PrivateKey, decode_secret_key, load_secret_key},
};
use tokio::sync::mpsc;

use crate::{
    session::{
        config::{AuthMethod, Session},
        ssh_keys::{
            authenticate_with_default_keys, normalize_inline_private_key, private_keys_with_algs,
            session_has_explicit_key,
        },
    },
    system::{
        RemotePort, RemoteProcess, SystemSnapshot, remote_ports_from_probe,
        remote_processes_from_ps, remote_snapshot_from_kv,
    },
    terminal::{BackendCommand, BackendEvent, BackendTx, GuardedBackendEventSender},
};

pub fn spawn_ssh_terminal(
    runtime: &tokio::runtime::Handle,
    tab_id: String,
    session: Session,
    cols: u16,
    rows: u16,
    events: GuardedBackendEventSender,
) -> BackendTx {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<BackendCommand>();
    let task_tab = tab_id.clone();
    runtime.spawn(async move {
        if let Err(err) = run_ssh(
            task_tab.clone(),
            session,
            cols,
            rows,
            cmd_rx,
            events.clone(),
        )
        .await
        {
            let _ = events.send(BackendEvent::Closed {
                tab_id: task_tab,
                reason: format!("{err:#}"),
            });
        }
    });
    BackendTx::Ssh(cmd_tx)
}

async fn sample_remote_system_with_handle(
    handle: Arc<tokio::sync::Mutex<russh::client::Handle<ClientHandler>>>,
) -> Result<SystemSnapshot> {
    let unix_result = async {
        let output = execute_remote_command_with_handle(
            handle.clone(),
            REMOTE_SYSTEM_PROBE,
            "Unix remote metrics probe",
        )
        .await?;
        remote_snapshot_from_kv(&output).context("parse Unix remote metrics probe")
    }
    .await;

    match unix_result {
        Ok(snapshot) => Ok(snapshot),
        Err(unix_error) => {
            let command = powershell_encoded_command(REMOTE_WINDOWS_SYSTEM_PROBE);
            let output = execute_remote_command_with_handle(
                handle,
                &command,
                "Windows remote metrics probe",
            )
            .await
            .with_context(|| format!("Unix probe failed first: {unix_error:#}"))?;
            remote_snapshot_from_kv(&output)
                .context("parse Windows remote metrics probe")
                .with_context(|| format!("Unix probe failed first: {unix_error:#}"))
        }
    }
}

async fn execute_remote_command_with_handle(
    handle: Arc<tokio::sync::Mutex<russh::client::Handle<ClientHandler>>>,
    command: &str,
    operation: &str,
) -> Result<String> {
    let mut channel = handle
        .lock()
        .await
        .channel_open_session()
        .await
        .with_context(|| format!("open {operation} session"))?;
    channel
        .exec(true, command)
        .await
        .with_context(|| format!("execute {operation}"))?;

    let mut output = Vec::new();
    let mut exit_status = None;
    let wait_result = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while let Some(msg) = channel.wait().await {
            match msg {
                ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, ext: _ } => {
                    output.extend_from_slice(&data);
                }
                ChannelMsg::ExitStatus {
                    exit_status: status,
                } => exit_status = Some(status),
                ChannelMsg::Close => break,
                _ => {}
            }
        }
    })
    .await;

    if wait_result.is_err() {
        return Err(anyhow!("{operation} timed out after 20 seconds"));
    }

    let output = String::from_utf8_lossy(&output).trim().to_string();
    if exit_status.is_none_or(|status| status != 0) {
        let detail = if output.is_empty() {
            exit_status.map_or_else(
                || "remote command closed without an exit status".to_string(),
                |status| format!("exit status {status}"),
            )
        } else {
            output
        };
        return Err(anyhow!("{operation} failed: {detail}"));
    }
    Ok(output)
}

async fn sample_remote_processes_with_handle(
    handle: Arc<tokio::sync::Mutex<russh::client::Handle<ClientHandler>>>,
) -> Result<Vec<RemoteProcess>> {
    let unix_result = async {
        let output = execute_remote_command_with_handle(
            handle.clone(),
            REMOTE_PROCESS_PROBE,
            "Unix remote process probe",
        )
        .await?;
        parse_remote_process_probe(&output, "Unix remote process probe")
    }
    .await;

    match unix_result {
        Ok(processes) => Ok(processes),
        Err(unix_error) => {
            let command = powershell_encoded_command(REMOTE_WINDOWS_PROCESS_PROBE);
            let output = execute_remote_command_with_handle(
                handle,
                &command,
                "Windows remote process probe",
            )
            .await
            .with_context(|| format!("Unix probe failed first: {unix_error:#}"))?;
            parse_remote_process_probe(&output, "Windows remote process probe")
                .with_context(|| format!("Unix probe failed first: {unix_error:#}"))
        }
    }
}

fn parse_remote_process_probe(output: &str, operation: &str) -> Result<Vec<RemoteProcess>> {
    let processes = remote_processes_from_ps(output);
    if !processes.is_empty() {
        return Ok(processes);
    }

    let detail = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("empty output");
    Err(anyhow!(
        "{operation} returned no parseable process rows: {detail}"
    ))
}

async fn sample_remote_ports_with_handle(
    handle: Arc<tokio::sync::Mutex<russh::client::Handle<ClientHandler>>>,
) -> Result<Vec<RemotePort>> {
    let unix_result = async {
        let output = execute_remote_command_with_handle(
            handle.clone(),
            REMOTE_PORT_PROBE,
            "Unix remote port probe",
        )
        .await?;
        parse_remote_port_probe(&output, "Unix remote port probe")
    }
    .await;

    match unix_result {
        Ok(ports) => Ok(ports),
        Err(unix_error) => {
            let command = powershell_encoded_command(REMOTE_WINDOWS_PORT_PROBE);
            let output =
                execute_remote_command_with_handle(handle, &command, "Windows remote port probe")
                    .await
                    .with_context(|| format!("Unix probe failed first: {unix_error:#}"))?;
            parse_remote_port_probe(&output, "Windows remote port probe")
                .with_context(|| format!("Unix probe failed first: {unix_error:#}"))
        }
    }
}

fn parse_remote_port_probe(output: &str, operation: &str) -> Result<Vec<RemotePort>> {
    let ports = remote_ports_from_probe(output);
    if !ports.is_empty() {
        return Ok(ports);
    }
    if output.trim().is_empty() {
        return Ok(Vec::new());
    }

    let detail = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("empty output");
    Err(anyhow!(
        "{operation} returned no parseable port rows: {detail}"
    ))
}

async fn terminate_remote_process_with_handle(
    handle: Arc<tokio::sync::Mutex<russh::client::Handle<ClientHandler>>>,
    pid: u32,
) -> Result<()> {
    if pid <= 1 {
        return Err(anyhow!("refusing to terminate protected PID {pid}"));
    }
    if let Err(unix_error) = execute_remote_command_with_handle(
        handle.clone(),
        &format!("kill -TERM {pid}"),
        "terminate Unix remote process",
    )
    .await
    {
        let command = powershell_encoded_command(&format!(
            "$ErrorActionPreference = 'Stop'; Stop-Process -Id {pid} -ErrorAction Stop"
        ));
        execute_remote_command_with_handle(handle, &command, "terminate Windows remote process")
            .await
            .with_context(|| format!("Unix termination failed first: {unix_error:#}"))?;
    }
    Ok(())
}

fn powershell_encoded_command(script: &str) -> String {
    let utf16_le = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16_le);
    format!("powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {encoded}")
}

async fn run_ssh(
    tab_id: String,
    session: Session,
    cols: u16,
    rows: u16,
    mut commands: mpsc::UnboundedReceiver<BackendCommand>,
    events: GuardedBackendEventSender,
) -> Result<()> {
    let _ = events.send(BackendEvent::Status {
        tab_id: tab_id.clone(),
        text: format!(
            "connecting {}@{}:{}...",
            session.user, session.host, session.port
        ),
    });

    let handle = Arc::new(tokio::sync::Mutex::new(
        connect_and_authenticate(&tab_id, &session, &events).await?,
    ));

    let mut channel = handle
        .lock()
        .await
        .channel_open_session()
        .await
        .context("open session")?;
    channel
        .request_pty(true, "xterm-256color", cols.into(), rows.into(), 0, 0, &[])
        .await
        .context("request pty")?;
    channel.request_shell(true).await.context("request shell")?;

    let _ = events.send(BackendEvent::Status {
        tab_id: tab_id.clone(),
        text: format!("connected {}@{}", session.user, session.host),
    });
    let _ = events.send(BackendEvent::Connected {
        tab_id: tab_id.clone(),
    });

    let exit_reason;
    let mut is_graceful_close = false;

    loop {
        tokio::select! {
            command = commands.recv() => {
                match command {
                    Some(BackendCommand::Input(bytes)) => {
                        if let Err(err) = channel.data(bytes.as_slice()).await {
                            tracing::error!("[ssh] write error on tab {}: {}", tab_id, err);
                            exit_reason = format!("ssh write error: {err}");
                            break;
                        }
                    }
                    Some(BackendCommand::Resize { cols, rows }) => {
                        let _ = channel.window_change(cols.into(), rows.into(), 0, 0).await;
                    }
                    Some(BackendCommand::SampleMetrics) => {
                        let handle_clone = handle.clone();
                        let tab_id_clone = tab_id.clone();
                        let events_clone = events.clone();
                        tokio::spawn(async move {
                            match sample_remote_system_with_handle(handle_clone).await {
                                Ok(snapshot) => {
                                    let _ = events_clone.send(BackendEvent::RemoteSystem {
                                        tab_id: tab_id_clone,
                                        snapshot,
                                    });
                                }
                                Err(err) => {
                                    let _ = events_clone.send(BackendEvent::RemoteSystemUnavailable {
                                        tab_id: tab_id_clone,
                                        reason: format!("remote metrics unavailable: {err:#}"),
                                    });
                                }
                            }
                        });
                    }
                    Some(BackendCommand::SampleProcesses) => {
                        let handle_clone = handle.clone();
                        let tab_id_clone = tab_id.clone();
                        let events_clone = events.clone();
                        tokio::spawn(async move {
                            match sample_remote_processes_with_handle(handle_clone).await {
                                Ok(processes) => {
                                    let _ = events_clone.send(BackendEvent::RemoteProcesses {
                                        tab_id: tab_id_clone,
                                        processes,
                                    });
                                }
                                Err(err) => {
                                    let _ = events_clone.send(
                                        BackendEvent::RemoteProcessesUnavailable {
                                            tab_id: tab_id_clone,
                                            reason: format!(
                                                "remote process list unavailable: {err:#}"
                                            ),
                                        },
                                    );
                                }
                            }
                        });
                    }
                    Some(BackendCommand::SamplePorts) => {
                        let handle_clone = handle.clone();
                        let tab_id_clone = tab_id.clone();
                        let events_clone = events.clone();
                        tokio::spawn(async move {
                            match sample_remote_ports_with_handle(handle_clone).await {
                                Ok(ports) => {
                                    let _ = events_clone.send(BackendEvent::RemotePorts {
                                        tab_id: tab_id_clone,
                                        ports,
                                    });
                                }
                                Err(err) => {
                                    let _ = events_clone.send(
                                        BackendEvent::RemotePortsUnavailable {
                                            tab_id: tab_id_clone,
                                            reason: format!(
                                                "remote port list unavailable: {err:#}"
                                            ),
                                        },
                                    );
                                }
                            }
                        });
                    }
                    Some(BackendCommand::TerminateProcess { pid }) => {
                        let handle_clone = handle.clone();
                        let tab_id_clone = tab_id.clone();
                        let events_clone = events.clone();
                        tokio::spawn(async move {
                            match terminate_remote_process_with_handle(handle_clone, pid).await {
                                Ok(()) => {
                                    let _ = events_clone.send(
                                        BackendEvent::RemoteProcessTerminated {
                                            tab_id: tab_id_clone,
                                            pid,
                                        },
                                    );
                                }
                                Err(err) => {
                                    let _ = events_clone.send(
                                        BackendEvent::RemoteProcessTerminateFailed {
                                            tab_id: tab_id_clone,
                                            pid,
                                            reason: format!("{err:#}"),
                                        },
                                    );
                                }
                            }
                        });
                    }
                    Some(BackendCommand::Close) | None => {
                        tracing::info!("[ssh] local client closed the session for tab {}", tab_id);
                        let _ = channel.eof().await;
                        exit_reason = "ssh session closed".to_string();
                        break;
                    }
                }
            }
            msg = channel.wait() => {
                match msg {
                    Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, ext: _ }) => {
                        let _ = events.send(BackendEvent::Output {
                            tab_id: tab_id.clone(),
                            bytes: data.to_vec(),
                        });
                    }
                    Some(ChannelMsg::ExitStatus { exit_status: _ }) | Some(ChannelMsg::Eof) => {
                        is_graceful_close = true;
                    }
                    Some(ChannelMsg::Close) => {
                        if is_graceful_close {
                            tracing::info!("[ssh] session gracefully closed by server for tab {}", tab_id);
                            exit_reason = "ssh session closed".to_string();
                        } else {
                            tracing::warn!("[ssh] connection abruptly closed by server for tab {}", tab_id);
                            exit_reason = "ssh connection lost (abrupt close)".to_string();
                        }
                        break;
                    }
                    None => {
                        if is_graceful_close {
                            tracing::info!("[ssh] network stream ended gracefully for tab {}", tab_id);
                            exit_reason = "ssh session closed".to_string();
                        } else {
                            tracing::warn!("[ssh] network drop detected for tab {}", tab_id);
                            exit_reason = "ssh connection lost (network drop)".to_string();
                        }
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    let _ = handle
        .lock()
        .await
        .disconnect(Disconnect::ByApplication, "bye", "")
        .await;
    let _ = events.send(BackendEvent::Closed {
        tab_id,
        reason: exit_reason,
    });
    Ok(())
}

async fn connect_and_authenticate(
    tab_id: &str,
    session: &Session,
    events: &GuardedBackendEventSender,
) -> Result<russh::client::Handle<ClientHandler>> {
    let config = Arc::new(crate::session::config::ssh_client_config());
    let addr = format!("{}:{}", session.host, session.port);
    tracing::info!(
        "[ssh] initiating tcp connection to {} (user: {})",
        addr,
        session.user
    );
    let status_text =
        if let Some((ptype, phost, pport)) = crate::session::config::active_proxy(session) {
            let pport_val = pport.unwrap_or_else(|| if ptype == "http" { 8080 } else { 1080 });
            format!(
                "connecting to {addr} via {} proxy {}:{}",
                ptype.to_uppercase(),
                phost,
                pport_val
            )
        } else {
            format!("opening tcp connection to {addr}")
        };
    let _ = events.send(BackendEvent::Status {
        tab_id: tab_id.to_string(),
        text: status_text,
    });
    let stream = crate::session::config::connect_proxy(session).await?;
    let mut handle = client::connect_stream(config, stream, ClientHandler)
        .await
        .with_context(|| format!("connect {addr} failed"))?;

    tracing::debug!("[ssh] tcp connected to {}", addr);

    let authed = match session.auth {
        AuthMethod::Password => {
            tracing::info!(
                "[ssh] sending password authentication for {}@{}",
                session.user,
                addr
            );
            let _ = events.send(BackendEvent::Status {
                tab_id: tab_id.to_string(),
                text: format!(
                    "connected to {addr}, sending password authentication for {}",
                    session.user
                ),
            });
            handle
                .authenticate_password(&session.user, &session.password)
                .await
                .context("password authentication failed")?
        }
        AuthMethod::Key => {
            let has_explicit_key = session_has_explicit_key(session);
            let source = if has_explicit_key {
                key_source_label(session)
            } else {
                "~/.ssh/ default keys".to_string()
            };
            tracing::info!(
                "[ssh] sending key authentication for {}@{} (key source: {})",
                session.user,
                addr,
                source
            );
            let _ = events.send(BackendEvent::Status {
                tab_id: tab_id.to_string(),
                text: if has_explicit_key {
                    format!("connected to {addr}, loading private key from {source}")
                } else {
                    format!(
                        "connected to {addr}, trying default keys from ~/.ssh/ for {}",
                        session.user
                    )
                },
            });

            let passphrase = session.passphrase.trim();
            let passphrase = (!passphrase.is_empty()).then_some(passphrase);

            if has_explicit_key {
                let keypair = load_session_private_key(session)?;
                let algorithm = format!("{:?}", keypair.algorithm());
                let _ = events.send(BackendEvent::Status {
                    tab_id: tab_id.to_string(),
                    text: format!("private key loaded from {source}, algorithm {algorithm}, sending public key authentication for {}", session.user),
                });
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
                                "[ssh] public key auth failed with algorithm, trying next"
                            );
                            continue;
                        }
                        Err(e) => {
                            tracing::debug!("[ssh] public key auth error: {:?}, trying next", e);
                            continue;
                        }
                    }
                }
                if !success {
                    return Err(anyhow::anyhow!(
                        "public key authentication failed for {}@{}:{} using {} ({})",
                        session.user,
                        session.host,
                        session.port,
                        source,
                        algorithm
                    ));
                }
                success
            } else {
                let success =
                    authenticate_with_default_keys(&mut handle, &session.user, passphrase).await?;
                if !success {
                    return Err(anyhow::anyhow!(
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
            // SSH Config auth: try the identity file from config, or default keys
            let source = key_source_label(session);
            tracing::info!(
                "[ssh] sending ssh-config authentication for {}@{} (key source: {})",
                session.user,
                addr,
                source
            );
            let _ = events.send(BackendEvent::Status {
                tab_id: tab_id.to_string(),
                text: format!("connected to {addr}, loading private key from {source}"),
            });

            // If an explicit key path is set from the SSH config IdentityFile, use it;
            // otherwise try default keys from ~/.ssh/
            // Note: for Config auth, we never use inline key content
            let has_explicit_key = !session.private_key_path.trim().is_empty();
            if has_explicit_key {
                let keypair = load_session_private_key(session)?;
                let algorithm = format!("{:?}", keypair.algorithm());
                let keys = private_keys_with_algs(keypair).context("invalid private key")?;
                let mut success = false;
                for key in keys {
                    match handle.authenticate_publickey(&session.user, key).await {
                        Ok(true) => {
                            success = true;
                            break;
                        }
                        Ok(false) => {
                            continue;
                        }
                        Err(_) => {
                            continue;
                        }
                    }
                }
                if !success {
                    return Err(anyhow::anyhow!(
                        "ssh-config key authentication failed for {}@{}:{} using {} ({})",
                        session.user,
                        session.host,
                        session.port,
                        source,
                        algorithm
                    ));
                }
                success
            } else {
                let passphrase = session.passphrase.trim();
                let passphrase = (!passphrase.is_empty()).then_some(passphrase);
                let _ = events.send(BackendEvent::Status {
                    tab_id: tab_id.to_string(),
                    text: format!(
                        "connected to {addr}, trying default keys from ~/.ssh/ for {}",
                        session.user
                    ),
                });
                let success =
                    authenticate_with_default_keys(&mut handle, &session.user, passphrase).await?;
                if !success {
                    return Err(anyhow::anyhow!(
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
        tracing::warn!("[ssh] authentication failed for {}@{}", session.user, addr);
        let _ = handle
            .disconnect(Disconnect::ByApplication, "auth failed", "")
            .await;
        return Err(anyhow!(
            "{}",
            match session.auth {
                AuthMethod::Password => format!(
                    "authentication failed: server rejected password authentication for {}@{}:{}",
                    session.user, session.host, session.port
                ),
                AuthMethod::Key => format!(
                    "authentication failed: server rejected public key authentication for {}@{}:{} using {}",
                    session.user,
                    session.host,
                    session.port,
                    key_source_label(session)
                ),
                AuthMethod::Config => format!(
                    "authentication failed: server rejected ssh-config authentication for {}@{}:{}",
                    session.user, session.host, session.port
                ),
            }
        ));
    }

    tracing::info!(
        "[ssh] authentication successful for {}@{}",
        session.user,
        addr
    );

    let _ = events.send(BackendEvent::Status {
        tab_id: tab_id.to_string(),
        text: format!(
            "authentication accepted, opening shell for {}@{}",
            session.user, session.host
        ),
    });

    Ok(handle)
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

fn key_source_label(session: &Session) -> String {
    let path = session.private_key_path.trim();
    let has_inline = !session.private_key_inline.trim().is_empty();
    match (!path.is_empty(), has_inline) {
        (true, true) => format!("inline key or {}", path),
        (true, false) => path.to_string(),
        (false, true) => "inline key text".to_string(),
        (false, false) => "unknown key source".to_string(),
    }
}

const REMOTE_SYSTEM_PROBE: &str = r#"sh -lc '
os=$(uname -s 2>/dev/null || echo unknown)
LC_ALL=C
export LC_ALL

if [ "$os" = "Linux" ] && [ -r /proc/stat ]; then
  cpu_stat() { awk '"'"'/^cpu / { total = ($2+$3+$4+$5+$6+$7+$8); printf "%.0f %.0f\n", total, $5 }'"'"' /proc/stat 2>/dev/null; }
  net_stat() { awk -F"[: ]+" '"'"'/:/ && $1!="Inter" && $1!="face" { rx += $3; tx += $11 } END { printf "%.0f %.0f\n", rx+0, tx+0 }'"'"' /proc/net/dev 2>/dev/null; }

  read cpu_total_1 cpu_idle_1 <<EOF
$(cpu_stat)
EOF
  read net_rx_1 net_tx_1 <<EOF
$(net_stat)
EOF
  sleep 1
  read cpu_total_2 cpu_idle_2 <<EOF
$(cpu_stat)
EOF
  read net_rx_2 net_tx_2 <<EOF
$(net_stat)
EOF

  cpu_total_1=${cpu_total_1:-0}
  cpu_idle_1=${cpu_idle_1:-0}
  cpu_total_2=${cpu_total_2:-0}
  cpu_idle_2=${cpu_idle_2:-0}
  net_rx_1=${net_rx_1:-0}
  net_tx_1=${net_tx_1:-0}
  net_rx_2=${net_rx_2:-0}
  net_tx_2=${net_tx_2:-0}
  cpu_delta=$((cpu_total_2 - cpu_total_1))
  idle_delta=$((cpu_idle_2 - cpu_idle_1))

  # metrics normalization
  cpu_percent=$(awk -v total="$cpu_delta" -v idle="$idle_delta" '"'"'BEGIN {
    if (total <= 0) {
      print "0.00"
    } else {
      usage = ((total-idle)/total)*100
      if (usage < 0) usage = 0
      if (usage > 100) usage = 100
      printf "%.2f", usage
    }
  }'"'"')
  mem_available=$(awk '"'"'
    /^MemTotal:/ { total = $2 * 1024 }
    /^MemAvailable:/ { available = $2 * 1024; has_available = 1 }
    /^MemFree:/ { free = $2 * 1024 }
    /^Buffers:/ { buffers = $2 * 1024 }
    /^Cached:/ { cached = $2 * 1024 }
    /^SReclaimable:/ { reclaimable = $2 * 1024 }
    /^Shmem:/ { shmem = $2 * 1024 }
    END {
      if (!has_available) available = free + buffers + cached + reclaimable - shmem
      if (available < 0) available = 0
      if (total > 0 && available > total) available = total
      printf "%.0f\n", available
    }
  '"'"' /proc/meminfo 2>/dev/null)
  mem_total=$(awk '"'"'/^MemTotal:/ {printf "%.0f\n", $2 * 1024; exit}'"'"' /proc/meminfo 2>/dev/null)
  swap_total=$(awk '"'"'/^SwapTotal:/ {printf "%.0f\n", $2 * 1024; exit}'"'"' /proc/meminfo 2>/dev/null)
  swap_free=$(awk '"'"'/^SwapFree:/ {printf "%.0f\n", $2 * 1024; exit}'"'"' /proc/meminfo 2>/dev/null)
  mem_used=$(( ${mem_total:-0} - ${mem_available:-0} ))
  [ "$mem_used" -lt 0 ] && mem_used=0
  swap_used=$(( ${swap_total:-0} - ${swap_free:-0} ))
  [ "$swap_used" -lt 0 ] && swap_used=0
  net_rx=$(( ${net_rx_2:-0} - ${net_rx_1:-0} ))
  [ "$net_rx" -lt 0 ] && net_rx=0
  net_tx=$(( ${net_tx_2:-0} - ${net_tx_1:-0} ))
  [ "$net_tx" -lt 0 ] && net_tx=0
  echo "CPU_PERCENT=${cpu_percent:-0.00}"
  echo "MEM_TOTAL=${mem_total:-0}"
  echo "MEM_USED=$mem_used"
  echo "SWAP_TOTAL=${swap_total:-0}"
  echo "SWAP_USED=$swap_used"
  echo "NET_RX=$net_rx"
  echo "NET_TX=$net_tx"
  LC_ALL=C df -kP 2>/dev/null | awk "NR > 1 && \$1 !~ /^(tmpfs|devtmpfs|ramfs|overlay|aufs)\$/ { printf \"DISK=%s\t%.0f\t%.0f\n\", \$6, \$4 * 1024, \$2 * 1024 }" | head -n 6
  exit 0
fi

if [ "$os" = "Darwin" ]; then
  net_stat() { netstat -ibn 2>/dev/null | awk '"'"'NR > 1 && $7 ~ /^[0-9]+$/ && $10 ~ /^[0-9]+$/ { rx += $7; tx += $10 } END { print rx+0, tx+0 }'"'"'; }

  read net_rx_1 net_tx_1 <<EOF
$(net_stat)
EOF
  sleep 1
  read net_rx_2 net_tx_2 <<EOF
$(net_stat)
EOF

  cpu_percent=$(top -l 2 -n 0 -s 1 2>/dev/null | awk -F"[:,% ]+" '"'"'/CPU usage:/ { user=$3; sys=$5 } END { if (user == "" && sys == "") print "0.00"; else printf "%.2f", user + sys }'"'"')
  mem_total=$(sysctl -n hw.memsize 2>/dev/null || echo 0)
  pagesize=$(sysctl -n hw.pagesize 2>/dev/null || echo 4096)
  vm_output=$(vm_stat 2>/dev/null)
  pages_active=$(printf "%s\n" "$vm_output" | awk '"'"'/Pages active/ { gsub("\\.","",$3); print $3+0 }'"'"')
  pages_wired=$(printf "%s\n" "$vm_output" | awk '"'"'/Pages wired down/ { gsub("\\.","",$4); print $4+0 }'"'"')
  pages_compressed=$(printf "%s\n" "$vm_output" | awk '"'"'/Pages occupied by compressor/ { gsub("\\.","",$5); print $5+0 }'"'"')
  pages_speculative=$(printf "%s\n" "$vm_output" | awk '"'"'/Pages speculative/ { gsub("\\.","",$3); print $3+0 }'"'"')
  mem_used=$(( (${pages_active:-0} + ${pages_wired:-0} + ${pages_compressed:-0} + ${pages_speculative:-0}) * ${pagesize:-4096} ))
  swap_line=$(sysctl vm.swapusage 2>/dev/null || true)
  swap_used=$(printf "%s\n" "$swap_line" | awk -F"[= ,]+" '"'"'
    function mult(unit) { return unit=="K"?1024:(unit=="M"?1048576:(unit=="G"?1073741824:(unit=="T"?1099511627776:1))) }
    /used/ { value=$4; unit=substr(value, length(value), 1); sub(/[A-Za-z]+$/, "", value); printf "%.0f", value * mult(unit) }'"'"')
  swap_total=$(printf "%s\n" "$swap_line" | awk -F"[= ,]+" '"'"'
    function mult(unit) { return unit=="K"?1024:(unit=="M"?1048576:(unit=="G"?1073741824:(unit=="T"?1099511627776:1))) }
    /used/ && /free/ { used=$4; free=$8; unit1=substr(used, length(used), 1); unit2=substr(free, length(free), 1); sub(/[A-Za-z]+$/, "", used); sub(/[A-Za-z]+$/, "", free); printf "%.0f", (used * mult(unit1)) + (free * mult(unit2)) }'"'"')

  echo "CPU_PERCENT=${cpu_percent:-0.00}"
  echo "MEM_TOTAL=${mem_total:-0}"
  echo "MEM_USED=${mem_used:-0}"
  echo "SWAP_TOTAL=${swap_total:-0}"
  echo "SWAP_USED=${swap_used:-0}"
  echo "NET_RX=$(( ${net_rx_2:-0} - ${net_rx_1:-0} ))"
  echo "NET_TX=$(( ${net_tx_2:-0} - ${net_tx_1:-0} ))"
  df -kP 2>/dev/null | awk "NR > 1 && \$1 !~ /^(devfs|tmpfs|devtmpfs|ramfs|overlay|aufs)\$/ { printf \"DISK=%s\t%s\t%s\n\", \$6, \$4 * 1024, \$2 * 1024 }" | head -n 6
  exit 0
fi

echo "unsupported Unix remote operating system: $os" >&2
exit 2
'"#;

const REMOTE_PROCESS_PROBE: &str = r#"sh -lc '
os=$(uname -s 2>/dev/null || echo unknown)
LC_ALL=C
export LC_ALL

if [ "$os" = "Linux" ] && [ -r /proc/stat ]; then
  before=$(mktemp "${TMPDIR:-/tmp}/ashell-process-before.XXXXXX") || exit 1
  after=$(mktemp "${TMPDIR:-/tmp}/ashell-process-after.XXXXXX") || { rm -f "$before"; exit 1; }
  trap '"'"'rm -f "$before" "$after"'"'"' EXIT HUP INT TERM
  hz=$(getconf CLK_TCK 2>/dev/null || echo 100)
  page_size=$(getconf PAGESIZE 2>/dev/null || echo 4096)

  capture_processes() {
    for process_stat in /proc/[0-9]*/stat; do
      [ -r "$process_stat" ] || continue
      process_pid=${process_stat#/proc/}
      process_pid=${process_pid%/stat}
      process_line=$(cat "$process_stat" 2>/dev/null) || continue
      process_rest=${process_line##*) }
      set -- $process_rest
      [ "$#" -ge 22 ] || continue
      process_ticks=$((${12} + ${13}))
      process_start=${20}
      process_rss_pages=${22}
      [ "$process_rss_pages" -ge 0 ] 2>/dev/null || process_rss_pages=0
      process_memory=$((process_rss_pages * page_size))
      process_user=$(stat -c %U "/proc/$process_pid" 2>/dev/null || echo "-")
      process_command=$(tr '"'"'\000\t\r\n'"'"' '"'"'    '"'"' < "/proc/$process_pid/cmdline" 2>/dev/null)
      if [ -z "$process_command" ]; then
        process_command=$(tr '"'"'\t\r\n'"'"' '"'"'   '"'"' < "/proc/$process_pid/comm" 2>/dev/null)
      fi
      [ -n "$process_command" ] || process_command="[$process_pid]"
      printf '"'"'%s\t%s\t%s\t%s\t%s\t%s\n'"'"' "$process_pid" "$process_user" "$process_ticks" "$process_start" "$process_memory" "$process_command"
    done
  }

  capture_processes > "$before"
  sleep 1
  capture_processes > "$after"
  awk -F '"'"'\t'"'"' -v hz="$hz" '"'"'
    NR == FNR { ticks[$1] = $3; starts[$1] = $4; next }
    {
      delta = ($1 in ticks && starts[$1] == $4) ? ($3 - ticks[$1]) : 0;
      cpu = (delta > 0 && hz > 0) ? delta * 100 / hz : 0;
      printf "PROCESS\t%s\t%s\t%.2f\t%s\t%s\n", $1, $2, cpu, $5, $6;
    }
  '"'"' "$before" "$after"
  exit 0
fi

if [ "$os" = "Darwin" ]; then
  top_output=$(mktemp "${TMPDIR:-/tmp}/ashell-process-top.XXXXXX") || exit 1
  trap '"'"'rm -f "$top_output"'"'"' EXIT HUP INT TERM
  LC_ALL=C top -l 2 -s 1 -n 10000 -stats pid,cpu > "$top_output" 2>/dev/null || exit 1
  LC_ALL=C ps -axo pid=,user=,rss=,command= 2>/dev/null | awk -v top_output="$top_output" '"'"'
    BEGIN {
      sample = 0;
      while ((getline line < top_output) > 0) {
        count = split(line, fields, /[[:space:]]+/);
        start = fields[1] == "" ? 2 : 1;
        if (fields[start] == "PID") { sample++; continue; }
        if (sample == 2 && fields[start] ~ /^[0-9]+$/) {
          value = fields[start + 1];
          gsub(/%/, "", value);
          cpu[fields[start]] = value + 0;
        }
      }
      close(top_output);
    }
    {
      pid = $1; user = $2; memory = $3 * 1024;
      $1 = $2 = $3 = "";
      sub(/^[[:space:]]+/, "");
      printf "PROCESS\t%s\t%s\t%.2f\t%.0f\t%s\n", pid, user, cpu[pid] + 0, memory, $0;
    }
  '"'"'
  exit 0
fi

echo "unsupported Unix remote operating system: $os" >&2
exit 2
'"#;

const REMOTE_PORT_PROBE: &str = r#"sh -lc '
LC_ALL=C
export LC_ALL
if command -v lsof >/dev/null 2>&1; then
  lsof_output=$(
  lsof -nP -iTCP -sTCP:LISTEN 2>/dev/null | awk '"'"'
    NR > 1 && $2 ~ /^[0-9]+$/ {
      endpoint = $9;
      state = $10;
      gsub(/[()]/, "", state);
      if (endpoint !~ /:[0-9]+$/) next;
      port = endpoint;
      sub(/^.*:/, "", port);
      address = endpoint;
      sub(/:[^:]*$/, "", address);
      if (address == "") address = "*";
      if (state == "") state = "LISTEN";
      printf "PORT\tTCP\t%s\t%s\t%s\t%s\t%s\n", address, port, state, $2, $1;
    }
  '"'"'
  lsof -nP -iUDP 2>/dev/null | awk '"'"'
    NR > 1 && $2 ~ /^[0-9]+$/ {
      endpoint = $9;
      if (endpoint !~ /:[0-9]+$/) next;
      port = endpoint;
      sub(/^.*:/, "", port);
      address = endpoint;
      sub(/:[^:]*$/, "", address);
      if (address == "") address = "*";
      printf "PORT\tUDP\t%s\t%s\tUNCONN\t%s\t%s\n", address, port, $2, $1;
    }
  '"'"'
  )
  if [ -n "$lsof_output" ]; then
    printf "%s\n" "$lsof_output"
    exit 0
  fi
fi

if command -v ss >/dev/null 2>&1; then
  ss_output=$(ss -lntup 2>/dev/null | awk '"'"'
    NF >= 5 {
      protocol = $1;
      state = $2;
      endpoint = $5;
      if (endpoint !~ /:[0-9]+$/) next;
      port = endpoint;
      sub(/^.*:/, "", port);
      address = endpoint;
      sub(/:[^:]*$/, "", address);
      pid = "-";
      process = "-";
      for (i = 6; i <= NF; i++) {
        token = $i;
        if (token ~ /users:/) {
          name = token;
          sub(/^.*\(\("/, "", name);
          sub(/".*$/, "", name);
          if (name != "") process = name;
          if (match(token, /pid=[0-9]+/)) {
            pid = substr(token, RSTART + 4, RLENGTH - 4);
          }
        }
      }
      printf "PORT\t%s\t%s\t%s\t%s\t%s\t%s\n", protocol, address, port, state, pid, process;
    }
  '"'"'
  )
  if [ -n "$ss_output" ]; then
    printf "%s\n" "$ss_output"
    exit 0
  fi
fi

if command -v netstat >/dev/null 2>&1; then
  netstat -an 2>/dev/null | awk '"'"'
    NR > 1 && NF >= 4 {
      protocol = $1;
      endpoint = $4;
      state = protocol ~ /^udp/i ? "UNCONN" : "LISTEN";
      if (endpoint !~ /:[0-9]+$/ && endpoint !~ /\.[0-9]+$/) next;
      port = endpoint;
      sub(/^.*[:.]/, "", port);
      address = endpoint;
      sub(/[:.][^:.]*$/, "", address);
      if (address == "") address = "*";
      if (protocol !~ /^udp/i && $NF != "LISTEN" && $NF != "LISTENING") next;
      printf "PORT\t%s\t%s\t%s\t%s\t-\t-\n", protocol, address, port, state;
    }
  '"'"'
  exit 0
fi

echo "no supported remote port utility found" >&2
exit 2
'"#;

const REMOTE_WINDOWS_SYSTEM_PROBE: &str = r#"$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$cpu = (Get-CimInstance Win32_Processor | Measure-Object -Property LoadPercentage -Average).Average
$os = Get-CimInstance Win32_OperatingSystem
$memTotal = [uint64]$os.TotalVisibleMemorySize * 1024
$memFree = [uint64]$os.FreePhysicalMemory * 1024
$swapTotal = [uint64]$os.TotalVirtualMemorySize * 1024 - $memTotal
$swapFree = [uint64]$os.FreeVirtualMemory * 1024 - $memFree
$network = Get-CimInstance Win32_PerfFormattedData_Tcpip_NetworkInterface
$netRx = ($network | Measure-Object -Property BytesReceivedPersec -Sum).Sum
$netTx = ($network | Measure-Object -Property BytesSentPersec -Sum).Sum
$cpuText = [string]::Format([Globalization.CultureInfo]::InvariantCulture, "{0:F2}", [double]$cpu)
Write-Output ("CPU_PERCENT={0}" -f $cpuText)
Write-Output ("MEM_TOTAL={0}" -f $memTotal)
Write-Output ("MEM_USED={0}" -f ($memTotal - $memFree))
Write-Output ("SWAP_TOTAL={0}" -f ([Math]::Max(0, $swapTotal)))
Write-Output ("SWAP_USED={0}" -f ([Math]::Max(0, $swapTotal - $swapFree)))
Write-Output ("NET_RX={0}" -f ([uint64]$netRx))
Write-Output ("NET_TX={0}" -f ([uint64]$netTx))
Get-CimInstance Win32_LogicalDisk -Filter "DriveType=3" | ForEach-Object {
  Write-Output ("DISK={0}`t{1}`t{2}" -f $_.DeviceID, [uint64]$_.FreeSpace, [uint64]$_.Size)
}"#;

const REMOTE_WINDOWS_PROCESS_PROBE: &str = r#"$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$before = @{}
Get-Process | ForEach-Object { if ($null -ne $_.CPU) { $before[$_.Id] = [double]$_.CPU } }
Start-Sleep -Seconds 1
Get-Process | ForEach-Object {
  $previous = $before[$_.Id]
  $cpu = if ($null -ne $previous -and $null -ne $_.CPU) { [Math]::Max(0, ([double]$_.CPU - $previous) * 100) } else { 0 }
  $command = $_.ProcessName.Replace("`t", " ").Replace("`r", " ").Replace("`n", " ")
  $cpuText = [string]::Format([Globalization.CultureInfo]::InvariantCulture, "{0:F2}", $cpu)
  Write-Output ("PROCESS`t{0}`t-`t{1}`t{2}`t{3}" -f $_.Id, $cpuText, [uint64]$_.WorkingSet64, $command)
}"#;

const REMOTE_WINDOWS_PORT_PROBE: &str = r#"$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$tab = [char]9
$processNames = @{}
Get-Process | ForEach-Object { $processNames[[int]$_.Id] = $_.ProcessName }
@(Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue) | ForEach-Object {
  $ownerPid = [int]$_.OwningProcess
  $processName = if ($processNames.ContainsKey($ownerPid)) { $processNames[$ownerPid] } else { '-' }
  Write-Output ("PORT{0}TCP{0}{1}{0}{2}{0}LISTEN{0}{3}{0}{4}" -f $tab, $_.LocalAddress, $_.LocalPort, $ownerPid, $processName)
}
@(Get-NetUDPEndpoint -ErrorAction SilentlyContinue) | ForEach-Object {
  $ownerPid = [int]$_.OwningProcess
  $processName = if ($processNames.ContainsKey($ownerPid)) { $processNames[$ownerPid] } else { '-' }
  Write-Output ("PORT{0}UDP{0}{1}{0}{2}{0}UNCONN{0}{3}{0}{4}" -f $tab, $_.LocalAddress, $_.LocalPort, $ownerPid, $processName)
}"#;

#[derive(Clone)]
struct ClientHandler;

#[async_trait]
impl Handler for ClientHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

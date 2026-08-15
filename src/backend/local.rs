use std::{
    io::{Read, Write},
    path::Path,
    sync::mpsc,
    thread,
    time::Duration,
};

#[cfg(not(windows))]
use std::time::Instant;

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
#[cfg(not(windows))]
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::terminal::{BackendCommand, BackendEvent, BackendTx, GuardedBackendEventSender};

#[cfg(not(windows))]
const DIRECTORY_POLL_INTERVAL: Duration = Duration::from_secs(1);

#[cfg(not(windows))]
fn local_process_directory(system: &mut System, pid: Pid) -> Option<std::path::PathBuf> {
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        false,
        ProcessRefreshKind::nothing().with_cwd(UpdateKind::Always),
    );
    system
        .process(pid)
        .and_then(|process| process.cwd())
        .map(std::path::PathBuf::from)
}

pub fn spawn_local_terminal_at(
    tab_id: String,
    cols: u16,
    rows: u16,
    events: GuardedBackendEventSender,
    initial_directory: Option<&Path>,
) -> Result<BackendTx> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("open local PTY")?;

    let shell = if cfg!(windows) {
        "powershell.exe".to_string()
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into())
    };

    let mut cmd = CommandBuilder::new(&shell);
    #[cfg(windows)]
    {
        const POWERSHELL_CWD_REPORTER: &str = r#"& {
            $global:AshellOriginalPrompt = $function:prompt
            function global:prompt {
                $promptText = if ($global:AshellOriginalPrompt) { & $global:AshellOriginalPrompt } else { "PS $PWD> " }
                $cwd = $PWD.ProviderPath
                $encoded = [Convert]::ToBase64String([System.Text.Encoding]::UTF8.GetBytes($cwd))
                [Console]::Write("$([char]27)]133;D$([char]7)$([char]27)]133;A$([char]7)")
                [Console]::Write("$([char]27)]0;ASHELL_CWD_B64:$encoded$([char]7)")
                "$promptText$([char]27)]133;B$([char]7)"
            }
        }"#;
        cmd.args(["-NoLogo", "-NoExit", "-Command", POWERSHELL_CWD_REPORTER]);
    }
    cmd.env(
        "TERM",
        std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into()),
    );
    cmd.env(
        "COLORTERM",
        std::env::var("COLORTERM").unwrap_or_else(|_| "truecolor".into()),
    );
    cmd.env("TERM_PROGRAM", "ashell");
    if let Ok(path) = std::env::var("PATH") {
        cmd.env("PATH", path);
    }
    if let Ok(lang) = std::env::var("LANG") {
        cmd.env("LANG", lang);
    } else {
        cmd.env("LANG", "en_US.UTF-8");
    }
    if let Ok(home) = std::env::var("HOME") {
        cmd.env("HOME", home);
    }
    if let Some(directory) = initial_directory.filter(|path| path.is_dir()) {
        cmd.cwd(directory.as_os_str());
    }
    cmd.env("SHELL", shell);
    let mut child = pair.slave.spawn_command(cmd).context("spawn local shell")?;
    #[cfg(not(windows))]
    let child_pid = child.process_id().map(Pid::from_u32);
    drop(pair.slave);

    let master = pair.master;
    let mut reader = master.try_clone_reader().context("clone PTY reader")?;
    let mut writer = master.take_writer().context("take PTY writer")?;
    let (cmd_tx, cmd_rx) = mpsc::channel::<BackendCommand>();

    let read_tab = tab_id.clone();
    let read_events = events.clone();
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = read_events.send(BackendEvent::Output {
                        tab_id: read_tab.clone(),
                        bytes: buf[..n].to_vec(),
                    });
                }
                Err(err) => {
                    let _ = read_events.send(BackendEvent::Closed {
                        tab_id: read_tab.clone(),
                        reason: format!("local read error: {err}"),
                    });
                    return;
                }
            }
        }
        let _ = read_events.send(BackendEvent::Closed {
            tab_id: read_tab,
            reason: "local shell closed".into(),
        });
    });

    let write_tab = tab_id.clone();
    let write_events = events.clone();
    thread::spawn(move || {
        #[cfg(not(windows))]
        let mut process_system = System::new();
        #[cfg(not(windows))]
        let mut last_directory = None;
        #[cfg(not(windows))]
        let mut last_directory_check = None;

        loop {
            match cmd_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(command) => match command {
                    BackendCommand::Input(bytes) => {
                        if let Err(err) = writer.write_all(&bytes) {
                            let _ = write_events.send(BackendEvent::Closed {
                                tab_id: write_tab.clone(),
                                reason: format!("local write error: {err}"),
                            });
                            break;
                        }
                        let _ = writer.flush();
                    }
                    BackendCommand::Resize { cols, rows } => {
                        let _ = master.resize(PtySize {
                            rows,
                            cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    }
                    BackendCommand::Close => break,
                    BackendCommand::SampleMetrics
                    | BackendCommand::SampleProcesses
                    | BackendCommand::SamplePorts
                    | BackendCommand::TerminateProcess { .. } => {}
                },
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Ok(Some(status)) = child.try_wait() {
                        let _ = write_events.send(BackendEvent::Closed {
                            tab_id: write_tab,
                            reason: format!("local shell exited: {status}"),
                        });
                        return;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }

            #[cfg(not(windows))]
            {
                if last_directory_check.is_none_or(|checked_at: Instant| {
                    checked_at.elapsed() >= DIRECTORY_POLL_INTERVAL
                }) {
                    last_directory_check = Some(Instant::now());
                    if let Some(directory) =
                        child_pid.and_then(|pid| local_process_directory(&mut process_system, pid))
                    {
                        if last_directory.as_ref() != Some(&directory) {
                            last_directory = Some(directory.clone());
                            let _ = write_events.send(BackendEvent::LocalDirectoryChanged {
                                tab_id: write_tab.clone(),
                                path: directory,
                            });
                        }
                    }
                }
            }
        }
        let _ = child.kill();
    });

    let _ = events.send(BackendEvent::Status {
        tab_id,
        text: "local shell ready".into(),
    });

    Ok(BackendTx::Local(cmd_tx))
}

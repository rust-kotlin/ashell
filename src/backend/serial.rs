use crate::session::config::Session;
use crate::terminal::{BackendCommand, BackendEvent, GuardedBackendEventSender};
use std::{
    io::{Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

#[derive(Clone)]
pub struct SerialHandle {
    commands: mpsc::Sender<BackendCommand>,
    owner: Arc<SerialOwner>,
}

struct SerialOwner(Arc<AtomicBool>);

impl Drop for SerialOwner {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl SerialHandle {
    pub fn send(&self, command: BackendCommand) -> Result<(), mpsc::SendError<BackendCommand>> {
        if matches!(command, BackendCommand::Close) {
            self.owner.0.store(true, Ordering::Release);
            let _ = self.commands.send(command);
            return Ok(());
        }
        if self.owner.0.load(Ordering::Acquire) {
            return Err(mpsc::SendError(command));
        }
        self.commands.send(command)
    }
}

/// Spawn the serial port backend threads.
/// Returns a sender to send commands (like keyboard inputs) to the serial port.
pub fn spawn_serial_client(
    _handle: &tokio::runtime::Handle,
    tab_id: String,
    session: Session,
    events_tx: GuardedBackendEventSender,
) -> SerialHandle {
    let (cmd_tx, cmd_rx) = mpsc::channel::<BackendCommand>();
    let stopped = Arc::new(AtomicBool::new(false));
    let owner = Arc::new(SerialOwner(stopped.clone()));

    let tab_id_clone = tab_id.clone();
    let events_tx_clone = events_tx.clone();

    std::thread::spawn(move || {
        let _ = events_tx_clone.send(BackendEvent::Status {
            tab_id: tab_id_clone.clone(),
            text: rust_i18n::t!("starting_connection").to_string(),
        });

        let port_name = session.host;
        let baud_rate = session.baud_rate;

        tracing::info!(
            "[serial] opening port {} at baud rate {}",
            port_name,
            baud_rate
        );

        let mut port_result = serialport::new(&port_name, baud_rate)
            .timeout(std::time::Duration::from_millis(100))
            .open();

        if port_result.is_err() && baud_rate != 0 {
            tracing::info!(
                "[serial] failed to open port with baud rate {}, retrying with 0 (virtual port mode)",
                baud_rate
            );
            port_result = serialport::new(&port_name, 0)
                .timeout(std::time::Duration::from_millis(100))
                .open();
        }

        let mut port = match port_result {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("[serial] failed to open port {}: {}", port_name, e);
                let _ = events_tx_clone.send(BackendEvent::Closed {
                    tab_id: tab_id_clone,
                    reason: format!("Failed to open serial port {port_name}: {e}"),
                });
                return;
            }
        };

        let mut port_write = match port.try_clone() {
            Ok(pw) => pw,
            Err(e) => {
                tracing::error!("[serial] failed to clone port: {}", e);
                let _ = events_tx_clone.send(BackendEvent::Closed {
                    tab_id: tab_id_clone,
                    reason: format!("Failed to clone serial port: {e}"),
                });
                return;
            }
        };

        // Notify connected
        let _ = events_tx_clone.send(BackendEvent::Connected {
            tab_id: tab_id_clone.clone(),
        });

        // Spawn write thread
        let tab_id_write = tab_id_clone.clone();
        let events_tx_write = events_tx_clone.clone();
        let write_stopped = stopped.clone();
        std::thread::spawn(move || {
            while !write_stopped.load(Ordering::Acquire) {
                let cmd = match cmd_rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(cmd) => cmd,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                match cmd {
                    BackendCommand::Input(bytes) => {
                        if let Err(e) = port_write.write_all(&bytes) {
                            tracing::error!("[serial] write error: {}", e);
                            let _ = events_tx_write.send(BackendEvent::Closed {
                                tab_id: tab_id_write.clone(),
                                reason: format!("Serial write error: {e}"),
                            });
                            break;
                        }
                        let _ = port_write.flush();
                    }
                    BackendCommand::Close => break,
                    BackendCommand::Resize { .. }
                    | BackendCommand::SampleMetrics
                    | BackendCommand::SampleProcesses
                    | BackendCommand::SamplePorts
                    | BackendCommand::TerminateProcess { .. } => {}
                }
            }
            write_stopped.store(true, Ordering::Release);
        });

        // Read loop in current thread
        let mut buf = [0u8; 1024];
        let mut last_was_cr = false;
        while !stopped.load(Ordering::Acquire) {
            match port.read(&mut buf) {
                Ok(n) if n > 0 => {
                    let mut processed = Vec::with_capacity(n * 2);
                    let read_bytes = &buf[..n];
                    for i in 0..n {
                        let b = read_bytes[i];
                        if b == b'\n' {
                            let prev_was_cr = if i > 0 {
                                read_bytes[i - 1] == b'\r'
                            } else {
                                last_was_cr
                            };
                            if !prev_was_cr {
                                processed.push(b'\r');
                            }
                        }
                        processed.push(b);
                    }
                    last_was_cr = read_bytes[n - 1] == b'\r';

                    let _ = events_tx_clone.send(BackendEvent::Output {
                        tab_id: tab_id_clone.clone(),
                        bytes: processed,
                    });
                }
                Ok(_) => {}
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => {
                    tracing::info!("[serial] port read error/closed: {}", e);
                    let _ = events_tx_clone.send(BackendEvent::Closed {
                        tab_id: tab_id_clone,
                        reason: format!("Serial read error: {e}"),
                    });
                    break;
                }
            }
        }
        stopped.store(true, Ordering::Release);
    });

    SerialHandle {
        commands: cmd_tx,
        owner,
    }
}

#[cfg(test)]
mod lifetime_tests {
    use super::*;

    #[test]
    fn close_signals_both_workers_without_waiting_for_the_command_queue() {
        let stopped = Arc::new(AtomicBool::new(false));
        let (commands, receiver) = mpsc::channel();
        let handle = SerialHandle {
            commands,
            owner: Arc::new(SerialOwner(stopped.clone())),
        };
        handle
            .send(BackendCommand::Input(b"queued".to_vec()))
            .unwrap();
        handle.send(BackendCommand::Close).unwrap();
        assert!(stopped.load(Ordering::Acquire));
        assert!(matches!(receiver.try_recv(), Ok(BackendCommand::Input(_))));
    }

    #[test]
    fn dropping_the_last_serial_handle_stops_the_workers() {
        let stopped = Arc::new(AtomicBool::new(false));
        let (commands, _receiver) = mpsc::channel();
        let first = SerialHandle {
            commands,
            owner: Arc::new(SerialOwner(stopped.clone())),
        };
        let last = first.clone();
        drop(first);
        assert!(!stopped.load(Ordering::Acquire));
        drop(last);
        assert!(stopped.load(Ordering::Acquire));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use portable_pty::{NativePtySystem, PtySize, PtySystem};

    #[tokio::test]
    async fn test_serial_read_write_simulation() {
        // 1. Create a PTY pair using portable-pty to simulate a serial device
        let pty_system = NativePtySystem::default();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();

        // On macOS/Linux, the slave name path behaves like a TTY device.
        let fd = pair.master.as_raw_fd().unwrap();
        let slave_name = unsafe {
            let ptr = libc::ptsname(fd);
            assert!(!ptr.is_null());
            std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
        };
        println!("Simulating serial device on PTY slave path: {}", slave_name);

        // 2. Spawn the serial backend targeting the PTY slave path
        let (events_tx, events_rx) = std::sync::mpsc::channel();
        let handle = tokio::runtime::Handle::current();
        let session = Session::serial(slave_name, 0);
        let backend_events = GuardedBackendEventSender::new(events_tx);
        let cmd_tx = spawn_serial_client(&handle, "test-tab".to_string(), session, backend_events);

        // Wait for the Status event
        let status_event = events_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("Failed to receive Status event")
            .into_current();
        match status_event {
            Some(BackendEvent::Status { tab_id, .. }) => assert_eq!(tab_id, "test-tab"),
            event => panic!("Expected Status event, got: {event:?}"),
        }

        // Wait for the Connected event
        let connected_event = events_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("Failed to receive Connected event")
            .into_current();
        match connected_event {
            Some(BackendEvent::Connected { tab_id }) => assert_eq!(tab_id, "test-tab"),
            event => panic!("Expected Connected event, got: {event:?}"),
        }

        // 3. Test Reading: Write to PTY master, verify serial backend outputs it to UI
        let mut master_writer = pair.master.take_writer().unwrap();
        master_writer.write_all(b"hello serial simulator").unwrap();
        master_writer.flush().unwrap();

        let output_event = events_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("Failed to receive Output event")
            .into_current();
        match output_event {
            Some(BackendEvent::Output { tab_id, bytes }) => {
                assert_eq!(tab_id, "test-tab");
                assert_eq!(bytes, b"hello serial simulator");
            }
            event => panic!("Expected Output event, got: {event:?}"),
        }

        // 4. Test Writing: Send BackendCommand::Input to backend, verify PTY master reads it
        cmd_tx
            .send(BackendCommand::Input(b"world serial simulator".to_vec()))
            .unwrap();

        let mut master_reader = pair.master.try_clone_reader().unwrap();
        let mut read_buf = [0u8; 128];
        let bytes_read = master_reader.read(&mut read_buf).unwrap();
        assert_eq!(&read_buf[..bytes_read], b"world serial simulator");

        // 5. Clean up
        cmd_tx.send(BackendCommand::Close).unwrap();
    }
}

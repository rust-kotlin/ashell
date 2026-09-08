use std::sync::mpsc::{SendError, Sender};

use crate::{
    backend::connection::ConnectionGuard,
    terminal::{BackendAttempt, BackendEvent, GuardedBackendEventSender},
};

/// Directory and connection events expire with their SFTP owner. Transfer
/// records remain addressable by UUID after that owner has been replaced.
#[derive(Clone)]
pub(super) struct SftpEventSender {
    transfers: Sender<BackendEvent>,
    connection: GuardedBackendEventSender,
}

impl SftpEventSender {
    pub(super) fn new(events: Sender<BackendEvent>) -> Self {
        Self {
            connection: GuardedBackendEventSender::new(events.clone()),
            transfers: events,
        }
    }

    pub(super) fn send(&self, event: BackendEvent) -> Result<(), Box<SendError<BackendEvent>>> {
        match event {
            BackendEvent::TransferStarted { .. }
            | BackendEvent::TransferProgress { .. }
            | BackendEvent::TransferInterrupted { .. } => {
                self.transfers.send(event).map_err(Box::new)
            }
            event => self.connection.send(event),
        }
    }

    pub(super) fn transfer_events(&self) -> &Sender<BackendEvent> {
        &self.transfers
    }

    pub(super) fn attempt(&self) -> BackendAttempt {
        self.connection.attempt()
    }
}

/// Only UI handles own this guard; a worker's final error stays visible until
/// the UI closes or replaces the connection, even if its socket has failed.
pub(super) struct SftpOwner {
    connection: ConnectionGuard,
    events: SftpEventSender,
}

impl SftpOwner {
    pub(super) fn new(connection: ConnectionGuard, events: SftpEventSender) -> Self {
        Self { connection, events }
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.connection.is_cancelled()
    }

    pub(super) fn cancel(&self) {
        self.events.connection.invalidate();
        self.connection.cancel();
    }
}

impl Drop for SftpOwner {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, mpsc};

    use crate::backend::connection::ConnectionControl;

    use super::*;

    #[test]
    fn replacing_sftp_discards_queued_directory_events_but_finishes_transfer_records() {
        let (sender, receiver) = mpsc::channel();
        let events = SftpEventSender::new(sender.clone());
        let owner = SftpOwner::new(ConnectionControl::new().guard(), events.clone());
        events
            .send(BackendEvent::SftpHome {
                tab_id: "tab".into(),
                home: "/old".into(),
            })
            .unwrap();
        owner.cancel();
        assert!(receiver.recv().unwrap().into_current().is_none());
        events
            .send(BackendEvent::SftpStatus {
                tab_id: "tab".into(),
                text: "old connection failed".into(),
            })
            .unwrap();
        assert!(receiver.try_recv().is_err());
        events
            .send(BackendEvent::TransferInterrupted {
                id: "old-transfer".into(),
                reason: "connection closed".into(),
            })
            .unwrap();
        assert!(matches!(
            receiver.recv().unwrap().into_current(),
            Some(BackendEvent::TransferInterrupted { id, .. }) if id == "old-transfer"
        ));

        let replacement = SftpEventSender::new(sender);
        replacement
            .send(BackendEvent::SftpHome {
                tab_id: "tab".into(),
                home: "/new".into(),
            })
            .unwrap();
        assert!(matches!(
            receiver.recv().unwrap().into_current(),
            Some(BackendEvent::SftpHome { home, .. }) if home == "/new"
        ));
    }

    #[test]
    fn verification_expires_when_the_last_sftp_owner_is_dropped() {
        let (sender, _receiver) = mpsc::channel();
        let events = SftpEventSender::new(sender);
        let attempt = events.attempt();
        let owner = Arc::new(SftpOwner::new(ConnectionControl::new().guard(), events));
        let last_owner = owner.clone();
        drop(owner);
        assert!(attempt.is_current());
        drop(last_owner);
        assert!(!attempt.is_current());
    }
}

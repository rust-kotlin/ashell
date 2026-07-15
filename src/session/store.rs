use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{self, Receiver, Sender};

use crate::{
    sftp::SftpHandle,
    terminal::{BackendCommand, BackendEvent, TerminalTab},
};

pub(crate) type SessionId = String;
pub(crate) type WindowOwnerId = u64;

#[allow(dead_code)]
pub(crate) struct SessionStore {
    sessions: HashMap<SessionId, TerminalTab>,
    sftp_handles: HashMap<SessionId, SftpHandle>,
    references: HashMap<SessionId, HashSet<WindowOwnerId>>,
    event_routes: HashMap<String, WindowOwnerId>,
    pending_events: HashMap<WindowOwnerId, VecDeque<BackendEvent>>,
    unrouted_events: HashMap<String, VecDeque<BackendEvent>>,
    events_tx: Sender<BackendEvent>,
    events_rx: Receiver<BackendEvent>,
}

#[allow(dead_code)]
impl SessionStore {
    pub(crate) fn new() -> Self {
        let (events_tx, events_rx) = mpsc::channel();
        Self {
            sessions: HashMap::new(),
            sftp_handles: HashMap::new(),
            references: HashMap::new(),
            event_routes: HashMap::new(),
            pending_events: HashMap::new(),
            unrouted_events: HashMap::new(),
            events_tx,
            events_rx,
        }
    }

    pub(crate) fn events_sender(&self) -> Sender<BackendEvent> {
        self.events_tx.clone()
    }

    pub(crate) fn insert_session(&mut self, tab: TerminalTab) -> SessionId {
        let session_id = tab.id.clone();
        self.sessions.insert(session_id.clone(), tab);
        session_id
    }

    pub(crate) fn insert_sftp(&mut self, session_id: SessionId, handle: SftpHandle) {
        self.sftp_handles.insert(session_id, handle);
    }

    pub(crate) fn attach(&mut self, session_id: &str, owner_id: WindowOwnerId) -> bool {
        if !self.sessions.contains_key(session_id) {
            return false;
        }
        self.references
            .entry(session_id.to_string())
            .or_default()
            .insert(owner_id)
    }

    pub(crate) fn move_reference(
        &mut self,
        session_id: &str,
        source_id: WindowOwnerId,
        target_id: WindowOwnerId,
    ) -> bool {
        let Some(owners) = self.references.get_mut(session_id) else {
            return false;
        };
        if !owners.remove(&source_id) {
            return false;
        }
        owners.insert(target_id);
        true
    }

    pub(crate) fn release(&mut self, session_id: &str, owner_id: WindowOwnerId) -> bool {
        let should_close = self
            .references
            .get_mut(session_id)
            .is_some_and(|owners| {
                owners.remove(&owner_id);
                owners.is_empty()
            });
        if !should_close {
            return false;
        }

        self.references.remove(session_id);
        if let Some(tab) = self.sessions.remove(session_id) {
            tab.send_backend(BackendCommand::Close);
        }
        if let Some(handle) = self.sftp_handles.remove(session_id) {
            handle.close();
        }
        true
    }

    pub(crate) fn register_event_route(&mut self, route_id: String, owner_id: WindowOwnerId) {
        self.event_routes.insert(route_id.clone(), owner_id);
        if let Some(events) = self.unrouted_events.remove(&route_id) {
            self.pending_events
                .entry(owner_id)
                .or_default()
                .extend(events);
        }
    }

    pub(crate) fn move_event_routes(
        &mut self,
        route_ids: &[String],
        source_id: WindowOwnerId,
        target_id: WindowOwnerId,
    ) -> bool {
        if route_ids
            .iter()
            .any(|route_id| self.event_routes.get(route_id).copied() != Some(source_id))
        {
            return false;
        }
        for route_id in route_ids {
            self.event_routes.insert(route_id.clone(), target_id);
        }

        let route_ids = route_ids.iter().map(String::as_str).collect::<HashSet<_>>();
        if let Some(mut source_events) = self.pending_events.remove(&source_id) {
            let mut retained = VecDeque::new();
            let mut moved = VecDeque::new();
            while let Some(event) = source_events.pop_front() {
                if event_route_id(&event).is_some_and(|route_id| route_ids.contains(route_id)) {
                    moved.push_back(event);
                } else {
                    retained.push_back(event);
                }
            }
            if !retained.is_empty() {
                self.pending_events.insert(source_id, retained);
            }
            if !moved.is_empty() {
                self.pending_events
                    .entry(target_id)
                    .or_default()
                    .extend(moved);
            }
        }
        true
    }

    pub(crate) fn remove_event_routes(&mut self, route_ids: &[String], owner_id: WindowOwnerId) {
        for route_id in route_ids {
            if self.event_routes.get(route_id).copied() == Some(owner_id) {
                self.event_routes.remove(route_id);
                self.unrouted_events.remove(route_id);
            }
        }
    }

    pub(crate) fn drain_events_for(&mut self, owner_id: WindowOwnerId) -> Vec<BackendEvent> {
        while let Ok(event) = self.events_rx.try_recv() {
            let Some(route_id) = event_route_id(&event) else {
                continue;
            };
            if let Some(owner_id) = self.event_routes.get(route_id).copied() {
                self.pending_events
                    .entry(owner_id)
                    .or_default()
                    .push_back(event);
            } else {
                self.unrouted_events
                    .entry(route_id.to_string())
                    .or_default()
                    .push_back(event);
            }
        }
        self.pending_events
            .remove(&owner_id)
            .map(VecDeque::into_iter)
            .map(Iterator::collect)
            .unwrap_or_default()
    }

    #[cfg(test)]
    fn owners(&self, session_id: &str) -> usize {
        self.references.get(session_id).map_or(0, HashSet::len)
    }
}

fn event_route_id(event: &BackendEvent) -> Option<&str> {
    match event {
        BackendEvent::Output { tab_id, .. }
        | BackendEvent::Status { tab_id, .. }
        | BackendEvent::Connected { tab_id }
        | BackendEvent::SftpEntries { tab_id, .. }
        | BackendEvent::SftpPreview { tab_id, .. }
        | BackendEvent::SftpStatus { tab_id, .. }
        | BackendEvent::RemoteSystem { tab_id, .. }
        | BackendEvent::RemoteSystemUnavailable { tab_id, .. }
        | BackendEvent::SftpHome { tab_id, .. }
        | BackendEvent::TransferProgress { tab_id, .. }
        | BackendEvent::TransferStarted { tab_id, .. }
        | BackendEvent::Closed { tab_id, .. }
        | BackendEvent::TerminalTitleChanged { tab_id, .. } => Some(tab_id),
        BackendEvent::SyncFinished(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::SessionStore;
    use crate::terminal::BackendEvent;

    #[test]
    fn moving_reference_preserves_single_owner() {
        let mut store = SessionStore::new();
        store
            .references
            .insert("session-a".to_string(), HashSet::from([1]));

        assert!(store.move_reference("session-a", 1, 2));
        assert_eq!(store.owners("session-a"), 1);
        assert_eq!(store.references["session-a"], HashSet::from([2]));
    }

    #[test]
    fn queued_event_follows_moved_route() {
        let mut store = SessionStore::new();
        store.register_event_route("session-a".to_string(), 1);
        store.move_event_routes(&["session-a".to_string()], 1, 2);
        store
            .events_sender()
            .send(BackendEvent::Status {
                tab_id: "session-a".to_string(),
                text: "connected".to_string(),
            })
            .unwrap();

        assert!(store.drain_events_for(1).is_empty());
        assert_eq!(store.drain_events_for(2).len(), 1);
    }

    #[test]
    fn already_queued_event_moves_with_its_route() {
        let mut store = SessionStore::new();
        store.register_event_route("session-a".to_string(), 1);
        store
            .events_sender()
            .send(BackendEvent::Status {
                tab_id: "session-a".to_string(),
                text: "connected".to_string(),
            })
            .unwrap();

        assert!(store.drain_events_for(99).is_empty());
        assert!(store.move_event_routes(&["session-a".to_string()], 1, 2));
        assert!(store.drain_events_for(1).is_empty());
        assert_eq!(store.drain_events_for(2).len(), 1);
    }

    #[test]
    fn event_sent_before_registration_is_not_lost() {
        let mut store = SessionStore::new();
        store
            .events_sender()
            .send(BackendEvent::Status {
                tab_id: "session-a".to_string(),
                text: "connecting".to_string(),
            })
            .unwrap();

        assert!(store.drain_events_for(1).is_empty());
        store.register_event_route("session-a".to_string(), 1);
        assert_eq!(store.drain_events_for(1).len(), 1);
    }

    #[test]
    fn failed_move_keeps_existing_owner() {
        let mut store = SessionStore::new();
        store
            .references
            .insert("session-a".to_string(), HashSet::from([1]));

        assert!(!store.move_reference("session-a", 3, 2));
        assert_eq!(store.owners("session-a"), 1);
        assert_eq!(store.references["session-a"], HashSet::from([1]));
    }
}
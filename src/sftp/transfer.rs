use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
        mpsc::Sender,
    },
    time::Duration,
};

use crate::{
    backend::connection::Cancellation,
    terminal::{BackendEvent, TransferState},
};

#[derive(Clone)]
pub(super) struct TransferQueue(Arc<tokio::sync::Semaphore>);

impl TransferQueue {
    pub(super) fn new(limit: usize) -> Self {
        Self(Arc::new(tokio::sync::Semaphore::new(limit)))
    }

    pub(super) async fn acquire(
        &self,
        flag: &TransferStateFlag,
    ) -> anyhow::Result<tokio::sync::OwnedSemaphorePermit> {
        tokio::select! {
            biased;
            _ = flag.cancelled() => Err(anyhow::anyhow!("transfer cancelled")),
            permit = self.0.clone().acquire_owned() => {
                permit.map_err(|_| anyhow::anyhow!("SFTP transfer queue closed"))
            }
        }
    }
}

#[derive(Clone)]
pub(super) struct TransferStateFlag {
    state: Arc<AtomicU8>,
    cancellation: Cancellation,
}

impl TransferStateFlag {
    pub(super) fn new() -> Self {
        Self {
            state: Arc::new(AtomicU8::new(0)),
            cancellation: Cancellation::new(),
        }
    }

    pub(super) fn pause(&self) {
        let _ = self
            .state
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst);
    }

    pub(super) fn resume(&self) {
        let _ = self
            .state
            .compare_exchange(1, 0, Ordering::SeqCst, Ordering::SeqCst);
    }

    pub(super) fn cancel(&self) {
        self.state.store(2, Ordering::SeqCst);
        self.cancellation.cancel();
    }

    pub(super) async fn cancelled(&self) {
        self.cancellation.cancelled().await;
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    pub(super) async fn run<T>(
        &self,
        operation: impl std::future::Future<Output = anyhow::Result<T>>,
    ) -> anyhow::Result<T> {
        tokio::select! {
            biased;
            _ = self.cancelled() => Err(anyhow::anyhow!("transfer cancelled")),
            result = operation => result,
        }
    }

    pub(super) async fn yield_if_paused(
        &self,
        events: &Sender<BackendEvent>,
        id: &str,
        transferred: u64,
        total: Option<u64>,
    ) -> anyhow::Result<()> {
        let mut was_paused = false;
        loop {
            match self.state.load(Ordering::SeqCst) {
                2 => return Err(anyhow::anyhow!("transfer cancelled")),
                1 => {
                    if !was_paused {
                        let _ = events.send(BackendEvent::TransferProgress {
                            id: id.to_string(),
                            transferred,
                            total,
                            state: TransferState::Paused,
                        });
                        was_paused = true;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                _ => {
                    if was_paused {
                        let _ = events.send(BackendEvent::TransferProgress {
                            id: id.to_string(),
                            transferred,
                            total,
                            state: TransferState::Running,
                        });
                    }
                    return Ok(());
                }
            }
        }
    }
}

/// Transfer controls belong to the worker that created the UUID, independently
/// of which tab or server is currently selected in the UI.
#[derive(Clone, Default)]
pub(super) struct TransferRegistry(Arc<Mutex<HashMap<String, TransferStateFlag>>>);

impl TransferRegistry {
    pub(super) fn flag(&self, id: &str) -> Option<TransferStateFlag> {
        self.0.lock().ok()?.get(id).cloned()
    }

    pub(super) fn register(
        &self,
        id: String,
        flag: TransferStateFlag,
        events: Sender<BackendEvent>,
    ) -> TransferCompletion {
        self.0
            .lock()
            .expect("transfer registry lock")
            .insert(id.clone(), flag);
        TransferCompletion {
            id,
            registry: self.clone(),
            events,
            finished: false,
        }
    }
}

/// Aborted workers must still finish their UI records, including futures that
/// were queued for a permit and never polled before their connection closed.
pub(super) struct TransferCompletion {
    id: String,
    registry: TransferRegistry,
    events: Sender<BackendEvent>,
    finished: bool,
}

impl TransferCompletion {
    pub(super) fn finish(&mut self) {
        self.finished = true;
    }
}

impl Drop for TransferCompletion {
    fn drop(&mut self) {
        if let Ok(mut transfers) = self.registry.0.lock() {
            if let Some(flag) = transfers.remove(&self.id) {
                if !self.finished {
                    flag.cancel();
                }
            }
        }
        if !self.finished {
            let _ = self.events.send(BackendEvent::TransferInterrupted {
                id: self.id.clone(),
                reason: rust_i18n::t!("transfer_connection_closed").to_string(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn queued_transfers_obey_the_limit_and_release_cancelled_waiters() {
        let queue = TransferQueue::new(1);
        let running = queue.acquire(&TransferStateFlag::new()).await.unwrap();
        let waiting_flag = TransferStateFlag::new();
        let waiting = queue.acquire(&waiting_flag);
        tokio::pin!(waiting);
        assert!(futures::poll!(&mut waiting).is_pending());

        waiting_flag.cancel();
        assert!(waiting.await.is_err());
        drop(running);
        assert!(queue.acquire(&TransferStateFlag::new()).await.is_ok());
    }

    #[tokio::test]
    async fn transfer_controls_stay_with_their_own_worker() {
        let first = TransferRegistry::default();
        let second = TransferRegistry::default();
        let (events, _received) = std::sync::mpsc::channel();
        let first_flag = TransferStateFlag::new();
        let second_flag = TransferStateFlag::new();
        let mut first_task = first.register("first".into(), first_flag.clone(), events.clone());
        let mut second_task = second.register("second".into(), second_flag.clone(), events);

        assert!(second.flag("first").is_none());
        first.flag("first").unwrap().cancel();
        first_flag.cancelled().await;
        assert!(!second_flag.is_cancelled());
        first_flag.resume();
        assert!(first_flag.is_cancelled());
        first_task.finish();
        second_task.finish();
    }

    #[tokio::test]
    async fn aborting_an_unstarted_task_interrupts_its_record_and_removes_controls() {
        let registry = TransferRegistry::default();
        let (events, received) = std::sync::mpsc::channel();
        let task = registry.register("upload".into(), TransferStateFlag::new(), events);
        assert!(registry.flag("upload").is_some());
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(async move {
            let _completion = task;
            std::future::pending::<()>().await;
        });
        tasks.shutdown().await;
        assert!(registry.flag("upload").is_none());
        assert!(
            matches!(received.try_recv(), Ok(BackendEvent::TransferInterrupted { id, .. }) if id == "upload")
        );
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn completed_tasks_do_not_emit_an_interruption_on_drop() {
        let registry = TransferRegistry::default();
        let (events, received) = std::sync::mpsc::channel();
        let mut task = registry.register("upload".into(), TransferStateFlag::new(), events);
        task.finish();
        drop(task);
        assert!(registry.flag("upload").is_none());
        assert!(received.try_recv().is_err());
    }
}

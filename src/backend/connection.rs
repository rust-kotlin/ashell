use std::{
    future::Future,
    io,
    ops::{Deref, DerefMut},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow};
use futures::task::AtomicWaker;
use russh::{Channel, ChannelStream, client};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::watch,
};

use crate::session::config::ProxyStream;

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const CHANNEL_OPEN_TIMEOUT: Duration = Duration::from_secs(10);
const CHANNEL_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// A cancellation signal that also wakes tasks blocked in network I/O.
#[derive(Clone)]
pub(crate) struct Cancellation(watch::Sender<bool>);

impl Cancellation {
    pub(crate) fn new() -> Self {
        Self(watch::channel(false).0)
    }

    pub(crate) fn cancel(&self) {
        self.0.send_replace(true);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    pub(crate) async fn cancelled(&self) {
        let mut receiver = self.0.subscribe();
        loop {
            let cancelled = *receiver.borrow_and_update();
            if cancelled {
                return;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }
}

struct ConnectionState {
    cancellation: Cancellation,
    transport: Mutex<Option<Box<dyn ProxyStream>>>,
    read_waker: AtomicWaker,
    write_waker: AtomicWaker,
}

/// Owns the socket separately from russh's background task, so cancellation
/// closes it even while SSH identification, key exchange or authentication hangs.
#[derive(Clone)]
pub(crate) struct ConnectionControl(Arc<ConnectionState>);

impl ConnectionControl {
    pub(crate) fn new() -> Self {
        Self(Arc::new(ConnectionState {
            cancellation: Cancellation::new(),
            transport: Mutex::new(None),
            read_waker: AtomicWaker::new(),
            write_waker: AtomicWaker::new(),
        }))
    }

    pub(crate) fn cancel(&self) {
        self.0.cancellation.cancel();
        self.close_transport();
    }

    fn close_transport(&self) {
        let transport = self
            .0
            .transport
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(transport);
        self.0.read_waker.wake();
        self.0.write_waker.wake();
    }

    pub(crate) async fn cancelled(&self) {
        self.0.cancellation.cancelled().await;
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.cancellation.is_cancelled()
    }

    pub(crate) fn guard(&self) -> ConnectionGuard {
        ConnectionGuard(self.clone())
    }

    pub(crate) fn attach(&self, stream: Box<dyn ProxyStream>) -> Result<ConnectionStream> {
        let mut transport = self
            .0
            .transport
            .lock()
            .map_err(|_| anyhow!("SSH transport lock poisoned"))?;
        if self.0.cancellation.is_cancelled() {
            return Err(anyhow!("SSH connection cancelled"));
        }
        if transport.is_some() {
            return Err(anyhow!("SSH transport already attached"));
        }
        *transport = Some(stream);
        Ok(ConnectionStream(self.clone()))
    }
}

/// Both the worker and its UI owner hold guards. Losing either closes the socket.
pub(crate) struct ConnectionGuard(ConnectionControl);

impl ConnectionGuard {
    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }

    pub(crate) fn cancel(&self) {
        self.0.cancel();
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub(crate) struct ConnectionStream(ConnectionControl);

impl ConnectionStream {
    fn poll_transport<T>(
        &self,
        poll: impl FnOnce(Pin<&mut dyn ProxyStream>) -> Poll<io::Result<T>>,
    ) -> Poll<io::Result<T>> {
        let Ok(mut transport) = self.0.0.transport.lock() else {
            return Poll::Ready(Err(io::Error::other("SSH transport lock poisoned")));
        };
        match transport.as_mut() {
            Some(stream) => poll(Pin::new(stream.as_mut())),
            None => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "SSH connection closed",
            ))),
        }
    }
}

impl AsyncRead for ConnectionStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.0.0.read_waker.register(cx.waker());
        self.poll_transport(|stream| stream.poll_read(cx, buffer))
    }
}

impl AsyncWrite for ConnectionStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.0.0.write_waker.register(cx.waker());
        self.poll_transport(|stream| stream.poll_write(cx, bytes))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.0.0.write_waker.register(cx.waker());
        self.poll_transport(|stream| stream.poll_flush(cx))
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.0.0.write_waker.register(cx.waker());
        self.poll_transport(|stream| stream.poll_shutdown(cx))
    }
}

impl Drop for ConnectionStream {
    fn drop(&mut self) {
        self.0.close_transport();
    }
}

pub(crate) struct SshConnection<H: client::Handler> {
    handle: Arc<client::Handle<H>>,
    control: ConnectionControl,
}

impl<H: client::Handler> SshConnection<H> {
    pub(crate) fn new(handle: client::Handle<H>, control: ConnectionControl) -> Self {
        Self {
            handle: Arc::new(handle),
            control,
        }
    }

    pub(crate) fn close(&self) {
        self.control.cancel();
    }

    /// Also observe an idle connection closing, rather than waiting for another
    /// file command before interrupting its transfers.
    pub(crate) async fn closed(&self) {
        while !self.handle.is_closed() {
            tokio::select! {
                _ = self.control.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_millis(250)) => {}
            }
        }
    }

    pub(crate) async fn open_session_channel(&self) -> Result<ClosingChannel>
    where
        H: 'static,
        H::Error: 'static,
    {
        let handle = self.handle.clone();
        let control = self.control.clone();
        // Keep the bounded protocol exchange alive if its caller cancels. Its
        // eventual channel is dropped and closed without disconnecting siblings.
        tokio::spawn(async move {
            match tokio::time::timeout(CHANNEL_OPEN_TIMEOUT, handle.channel_open_session()).await {
                Ok(result) => Ok(ClosingChannel {
                    channel: Some(result.context("open SSH session channel")?),
                    control,
                }),
                Err(_) => {
                    control.cancel();
                    Err(anyhow!("opening SSH session channel timed out"))
                }
            }
        })
        .await
        .context("SSH channel setup task failed")?
    }
}

impl<H: client::Handler> Drop for SshConnection<H> {
    fn drop(&mut self) {
        self.close();
    }
}

/// Raw russh channels do not close on Drop. Keep every exec channel guarded,
/// including when a timeout or cancellation drops its command future.
pub(crate) struct ClosingChannel {
    channel: Option<Channel<client::Msg>>,
    control: ConnectionControl,
}

impl ClosingChannel {
    pub(crate) fn into_stream(mut self) -> ChannelStream<client::Msg> {
        // russh's stream adapter supplies its own channel-close drop guard.
        self.channel
            .take()
            .expect("owned SSH channel")
            .into_stream()
    }
}

impl Deref for ClosingChannel {
    type Target = Channel<client::Msg>;

    fn deref(&self) -> &Self::Target {
        self.channel.as_ref().expect("owned SSH channel")
    }
}

impl DerefMut for ClosingChannel {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.channel.as_mut().expect("owned SSH channel")
    }
}

impl Drop for ClosingChannel {
    fn drop(&mut self) {
        let Some(channel) = self.channel.take() else {
            return;
        };
        let control = self.control.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if !matches!(
                    tokio::time::timeout(CHANNEL_CLOSE_TIMEOUT, channel.close()).await,
                    Ok(Ok(()))
                ) {
                    control.cancel();
                }
            });
        } else {
            control.cancel();
        }
    }
}

/// Bound the whole connection setup, including SSH identification and auth.
pub(crate) async fn connect_with_timeout<T>(future: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(CONNECT_TIMEOUT, future)
        .await
        .context("SSH connection setup timed out after 30 seconds")?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt as _;

    #[tokio::test]
    async fn cancellation_closes_the_socket_while_the_ssh_consumer_is_still_alive() {
        let control = ConnectionControl::new();
        let (socket, mut peer) = tokio::io::duplex(64);
        let mut consumer = control.attach(Box::new(socket)).unwrap();
        control.cancel();

        let mut buffer = [0; 1];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), peer.read(&mut buffer))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        assert_eq!(
            consumer.read(&mut buffer).await.unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
    }

    #[tokio::test]
    async fn cancellation_wakes_a_pending_ssh_identification_read() {
        let control = ConnectionControl::new();
        let (socket, _peer) = tokio::io::duplex(64);
        let mut consumer = control.attach(Box::new(socket)).unwrap();
        let mut buffer = [0; 1];
        let read = consumer.read(&mut buffer);
        tokio::pin!(read);
        assert!(futures::poll!(&mut read).is_pending());
        control.cancel();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), read)
                .await
                .unwrap()
                .is_err()
        );
    }

    #[tokio::test]
    async fn dropping_the_last_ui_owner_closes_a_still_running_worker() {
        let control = ConnectionControl::new();
        let _worker = control.guard();
        let owner = Arc::new(control.guard());
        let last_owner = owner.clone();
        let (socket, mut peer) = tokio::io::duplex(64);
        let _consumer = control.attach(Box::new(socket)).unwrap();
        drop(owner);
        assert!(!control.0.cancellation.is_cancelled());
        drop(last_owner);
        let mut buffer = [0; 1];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), peer.read(&mut buffer))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn a_late_tcp_result_cannot_attach_after_the_tab_was_closed() {
        let control = ConnectionControl::new();
        control.cancel();
        let (socket, mut peer) = tokio::io::duplex(64);
        assert!(control.attach(Box::new(socket)).is_err());
        let mut buffer = [0; 1];
        assert_eq!(peer.read(&mut buffer).await.unwrap(), 0);
    }
}

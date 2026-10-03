//! Process-wide shutdown signal for ending long-lived server streams.
//!
//! gRPC server-streaming RPCs (Registry `Watch`, `LeaseService::WatchExpiry`,
//! EventMgr `Receive`, Space `Notify`) never finish on their own, so a
//! graceful drain would otherwise wait on them until the grace period
//! expires. Each such service holds a [`ShutdownSignal`] and wraps its
//! returned stream with [`ShutdownSignal::end_stream`], which terminates the
//! stream (with a caller-supplied final item, typically
//! `Status::unavailable`) as soon as shutdown begins.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::sync::watch;

/// Fires every associated [`ShutdownSignal`].
pub struct ShutdownTrigger(watch::Sender<bool>);

impl ShutdownTrigger {
    pub fn trigger(&self) {
        let _ = self.0.send(true);
    }
}

/// Cheap-to-clone receiver side. [`ShutdownSignal::never`] is the default
/// for services built without shutdown wiring (tests, embedded use).
#[derive(Clone)]
pub struct ShutdownSignal(watch::Receiver<bool>);

pub fn channel() -> (ShutdownTrigger, ShutdownSignal) {
    let (tx, rx) = watch::channel(false);
    (ShutdownTrigger(tx), ShutdownSignal(rx))
}

impl Default for ShutdownSignal {
    fn default() -> Self {
        Self::never()
    }
}

impl ShutdownSignal {
    /// A signal that never fires.
    pub fn never() -> Self {
        let (_tx, rx) = watch::channel(false);
        // Sender dropped: `wait` then pends forever (see below).
        Self(rx)
    }

    /// Resolves once shutdown has been triggered; pends forever if the
    /// trigger was dropped without firing.
    pub async fn wait(mut self) {
        if self.0.wait_for(|v| *v).await.is_err() {
            std::future::pending::<()>().await;
        }
    }

    /// Wrap `stream` so it yields `final_item()` and then ends when shutdown
    /// begins.
    pub fn end_stream<S, F>(&self, stream: S, final_item: F) -> EndOnShutdown<S, F>
    where
        S: futures_core::Stream + Unpin,
        F: FnOnce() -> S::Item + Unpin,
    {
        EndOnShutdown {
            inner: stream,
            fired: Box::pin(self.clone().wait()),
            final_item: Some(final_item),
            done: false,
        }
    }
}

pub struct EndOnShutdown<S, F> {
    inner: S,
    fired: Pin<Box<dyn Future<Output = ()> + Send>>,
    final_item: Option<F>,
    done: bool,
}

impl<S, F> futures_core::Stream for EndOnShutdown<S, F>
where
    S: futures_core::Stream + Unpin,
    F: FnOnce() -> S::Item + Unpin,
{
    type Item = S::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        if this.done {
            return Poll::Ready(None);
        }
        if this.fired.as_mut().poll(cx).is_ready() {
            this.done = true;
            return Poll::Ready(this.final_item.take().map(|f| f()));
        }
        Pin::new(&mut this.inner).poll_next(cx)
    }
}

//! The fair buffer worker: dispatches queued requests and rotates counts.

use std::{hash::Hash, sync::Arc, time::Duration};

use tower::{Service, ServiceExt};
use tracing::Instrument;

use super::{message::Message, queue::Shared, BoxError};

/// Task that dispatches the fair buffer's queued requests to the inner
/// service, lowest priority key first, and rotates the recent request counts
/// on a fixed interval.
///
/// Run it with [`Worker::run`], either spawned by [`FairBuffer::new`] or
/// manually after [`FairBuffer::pair`].
///
/// Dropping the worker (or its [`run`](Worker::run) future) shuts the fair
/// buffer down: queued and subsequent requests fail instead of hanging.
///
/// [`FairBuffer::new`]: crate::FairBuffer::new
/// [`FairBuffer::pair`]: crate::FairBuffer::pair
pub struct Worker<T, K, R>
where
    T: Service<R>,
    T::Future: Send + 'static,
    R: Send + 'static,
{
    /// The wrapped service that processes requests.
    service: T,

    /// The queue and state shared with the `FairBuffer` handles.
    shared: Arc<Shared<K, R, T::Future>>,

    /// The period after which the recent request counts rotate.
    rotation_interval: Duration,
}

impl<T, K, R> Worker<T, K, R>
where
    T: Service<R>,
    T::Future: Send + 'static,
    T::Error: Into<BoxError>,
    K: Eq + Hash,
    R: Send + 'static,
{
    /// Creates a new fair buffer worker.
    ///
    /// See [`FairBuffer::pair`](crate::FairBuffer::pair) for details.
    pub(crate) fn new(
        service: T,
        shared: Arc<Shared<K, R, T::Future>>,
        rotation_interval: Duration,
    ) -> Self {
        Self {
            service,
            shared,
            rotation_interval,
        }
    }

    /// Runs the fair buffer worker until the inner service fails or this
    /// future is dropped.
    pub async fn run(mut self) {
        // Rotate on a fixed period, starting one period from now: this skips
        // a Tokio interval's immediate first tick, and `Delay` stops ticks
        // bunching up after a busy worker misses some.
        let start = tokio::time::Instant::now() + self.rotation_interval;
        let mut rotation = tokio::time::interval_at(start, self.rotation_interval);
        rotation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            // Dispatch all queued messages, lowest priority key first.
            while let Some(message) = self.shared.pop_lowest() {
                let Message { request, tx, span } = message;

                // Wait for the inner service inside the request's span.
                match self.service.ready().instrument(span.clone()).await {
                    Ok(service) => {
                        // Call the inner service inside the request's span,
                        // and send the response future back to the caller.
                        //
                        // A send error means the request was canceled
                        // in-between, so the response future is just dropped.
                        let response = span.in_scope(|| {
                            tracing::trace!("dispatching request to the inner service");
                            service.call(request)
                        });
                        let _ = tx.send(Ok(response));
                    }
                    Err(error) => {
                        let error = error.into();
                        tracing::debug!(%error, "fair buffered service failed");

                        // Fail the queued messages and reject future pushes...
                        let service_error = self.shared.fail(error);
                        // ...and fail the message we were about to dispatch.
                        let _ = tx.send(Err(service_error.into()));

                        return;
                    }
                }
            }

            tokio::select! {
                // Wait for a new message. A push between `pop_lowest`
                // returning `None` and this wait stores a wakeup permit, so
                // wakeups can't be lost.
                _ = self.shared.pushed() => {}

                // Or rotate the recent request counts.
                _ = rotation.tick() => self.shared.rotate_counts(),
            }
        }
    }
}

impl<T, K, R> Drop for Worker<T, K, R>
where
    T: Service<R>,
    T::Future: Send + 'static,
    R: Send + 'static,
{
    fn drop(&mut self) {
        tracing::trace!("dropping fair buffer worker");

        // Shut the fair buffer down, unless it already failed: queued
        // messages would otherwise leave their response futures pending
        // forever, because no other task drains the shared queue.
        self.shared.close();
    }
}

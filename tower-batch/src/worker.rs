use super::{
    error::{Closed, ServiceError},
    message::{self, Message},
    BatchControl,
};
use futures::future::TryFutureExt;
use pin_project::pin_project;
use std::sync::{Arc, Mutex};
use tokio::{
    stream::StreamExt,
    sync::mpsc,
    time::{delay_for, Delay},
};
use tower::{Service, ServiceExt};
use tracing_futures::Instrument;

/// Task that handles processing the buffer. This type should not be used
/// directly, instead `Buffer` requires an `Executor` that can accept this task.
///
/// The struct is `pub` in the private module and the type is *not* re-exported
/// as part of the public API. This is the "sealed" pattern to include "private"
/// types in public traits that are not meant for consumers of the library to
/// implement (only call).
#[pin_project]
#[derive(Debug)]
pub struct Worker<T, Request>
where
    T: Service<BatchControl<Request>>,
    T::Error: Into<crate::BoxError>,
{
    rx: mpsc::Receiver<Message<Request, T::Future>>,
    service: T,
    failed: Option<ServiceError>,
    handle: Handle,
    max_items: usize,
    max_latency: std::time::Duration,
}

/// Get the error out
#[derive(Debug)]
pub(crate) struct Handle {
    inner: Arc<Mutex<Option<ServiceError>>>,
}

impl<T, Request> Worker<T, Request>
where
    T: Service<BatchControl<Request>>,
    T::Error: Into<crate::BoxError>,
{
    pub(crate) fn new(
        service: T,
        rx: mpsc::Receiver<Message<Request, T::Future>>,
        max_items: usize,
        max_latency: std::time::Duration,
    ) -> (Handle, Worker<T, Request>) {
        let handle = Handle {
            inner: Arc::new(Mutex::new(None)),
        };

        let worker = Worker {
            rx,
            service,
            handle: handle.clone(),
            failed: None,
            max_items,
            max_latency,
        };

        (handle, worker)
    }

    async fn process_req(&mut self, req: Request, tx: message::Tx<T::Future>) {
        if let Some(ref failed) = self.failed {
            tracing::trace!("notifying caller about worker failure");
            let _ = tx.send(Err(failed.clone()));
        } else {
            match self.service.ready_and().await {
                Ok(svc) => {
                    let rsp = svc.call(req.into());
                    let _ = tx.send(Ok(rsp));
                }
                Err(e) => {
                    self.failed(e.into());
                    let _ = tx.send(Err(self
                        .failed
                        .as_ref()
                        .expect("Worker::failed did not set self.failed?")
                        .clone()));
                }
            }
        }
    }

    async fn flush_service(&mut self) {
        if let Err(e) = self
            .service
            .ready_and()
            .and_then(|svc| svc.call(BatchControl::Flush))
            .await
        {
            self.failed(e.into());
        }
    }

    pub async fn run(mut self) {
        use futures::future::Either::{Left, Right};
        // The timer is started when the first entry of a new batch is
        // submitted, so that the batch latency of all entries is at most
        // self.max_latency. However, we don't keep the timer running unless
        // there is a pending request to prevent wakeups on idle services.
        let mut timer: Option<Delay> = None;
        let mut pending_items = 0usize;
        loop {
            match timer {
                None => match self.rx.next().await {
                    // The first message in a new batch.
                    Some(msg) => {
                        let span = msg.span;
                        self.process_req(msg.request, msg.tx)
                            // Apply the provided span to request processing
                            .instrument(span)
                            .await;
                        timer = Some(delay_for(self.max_latency));
                        pending_items = 1;
                    }
                    // No more messages, ever.
                    None => return,
                },
                Some(delay) => {
                    // Wait on either a new message or the batch timer.
                    match futures::future::select(self.rx.next(), delay).await {
                        Left((Some(msg), delay)) => {
                            let span = msg.span;
                            self.process_req(msg.request, msg.tx)
                                // Apply the provided span to request processing.
                                .instrument(span)
                                .await;
                            pending_items += 1;
                            // Check whether we have too many pending items.
                            if pending_items >= self.max_items {
                                // XXX(hdevalence): what span should instrument this?
                                self.flush_service().await;
                                // Now we have an empty batch.
                                timer = None;
                                pending_items = 0;
                            } else {
                                // The timer is still running, set it back!
                                timer = Some(delay);
                            }
                        }
                        // No more messages, ever.
                        Left((None, _delay)) => {
                            return;
                        }
                        // The batch timer elapsed.
                        Right(((), _next)) => {
                            // XXX(hdevalence): what span should instrument this?
                            self.flush_service().await;
                            timer = None;
                            pending_items = 0;
                        }
                    }
                }
            }
        }
    }

    fn failed(&mut self, error: crate::BoxError) {
        // The underlying service failed when we called `poll_ready` on it with the given `error`. We
        // need to communicate this to all the `Buffer` handles. To do so, we wrap up the error in
        // an `Arc`, send that `Arc<E>` to all pending requests, and store it so that subsequent
        // requests will also fail with the same error.

        // Note that we need to handle the case where some handle is concurrently trying to send us
        // a request. We need to make sure that *either* the send of the request fails *or* it
        // receives an error on the `oneshot` it constructed. Specifically, we want to avoid the
        // case where we send errors to all outstanding requests, and *then* the caller sends its
        // request. We do this by *first* exposing the error, *then* closing the channel used to
        // send more requests (so the client will see the error when the send fails), and *then*
        // sending the error to all outstanding requests.
        let error = ServiceError::new(error);

        let mut inner = self.handle.inner.lock().unwrap();

        if inner.is_some() {
            // Future::poll was called after we've already errored out!
            return;
        }

        *inner = Some(error.clone());
        drop(inner);

        self.rx.close();

        // By closing the mpsc::Receiver, we know that that the run() loop will
        // drain all pending requests. We just need to make sure that any
        // requests that we receive before we've exhausted the receiver receive
        // the error:
        self.failed = Some(error);
    }
}

impl Handle {
    pub(crate) fn get_error_on_closed(&self) -> crate::BoxError {
        self.inner
            .lock()
            .unwrap()
            .as_ref()
            .map(|svc_err| svc_err.clone().into())
            .unwrap_or_else(|| Closed::new().into())
    }
}

impl Clone for Handle {
    fn clone(&self) -> Handle {
        Handle {
            inner: self.inner.clone(),
        }
    }
}

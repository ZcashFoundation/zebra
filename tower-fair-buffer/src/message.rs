//! Fair buffer message types.

use tokio::sync::oneshot;

use crate::BoxError;

/// Message sent to the fair buffer worker.
#[derive(Debug)]
pub(crate) struct Message<Request, Fut> {
    pub(crate) request: Request,
    pub(crate) tx: Tx<Fut>,
    pub(crate) span: tracing::Span,
}

/// Response sender.
///
/// Unlike tower's buffer, the error payload is a [`BoxError`] rather than a
/// shared `ServiceError`, so individual messages can fail with per-request
/// errors like [`Shed`](crate::error::Shed).
pub(crate) type Tx<Fut> = oneshot::Sender<Result<Fut, BoxError>>;

/// Response receiver.
pub(crate) type Rx<Fut> = oneshot::Receiver<Result<Fut, BoxError>>;

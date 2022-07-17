//! Batch message types.

use tokio::sync::{oneshot, OwnedSemaphorePermit};

use super::error::ServiceError;

/// Message sent to the batch worker
#[derive(Debug)]
pub(crate) struct Message<Request, Fut> {
    pub(crate) request: Request,
    pub(crate) tx: Tx<Fut>,
    pub(crate) span: tracing::Span,
    pub(super) _permit: OwnedSemaphorePermit,
}

/// Response sender
pub(crate) type Tx<Fut> = oneshot::Sender<Result<Fut, ServiceError>>;

/// Response receiver
pub(crate) type Rx<Fut> = oneshot::Receiver<Result<Fut, ServiceError>>;

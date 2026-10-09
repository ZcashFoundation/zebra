//! Batch message types.

use tokio::sync::{oneshot, OwnedSemaphorePermit};

use super::error::ServiceError;

/// Message sent to the batch worker
#[derive(Debug)]
pub(crate) enum Message<Request, Fut> {
    Item {
        request: Request,
        tx: Tx<Fut>,
        span: tracing::Span,
        _permit: OwnedSemaphorePermit,
    },
    Flush {
        span: tracing::Span,
        _permit: OwnedSemaphorePermit,
    },
}

/// Response sender
pub(crate) type Tx<Fut> = oneshot::Sender<Result<Fut, ServiceError>>;

/// Response receiver
pub(crate) type Rx<Fut> = oneshot::Receiver<Result<Fut, ServiceError>>;

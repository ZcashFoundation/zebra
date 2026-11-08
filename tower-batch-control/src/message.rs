//! Batch message types.

use tokio::sync::{oneshot, OwnedSemaphorePermit};

use super::{error::ServiceError, BatchControl, RequestWeight};

/// Message sent to the batch worker
#[derive(Debug)]
pub(crate) struct Message<Request: RequestWeight, Fut> {
    pub(crate) request: BatchControl<Request>,
    // Portions copyright (c) 2026 Zakura Contributors, licensed under the MIT license or the Apache
    // License, Version 2.0, at your option.
    // Adapted from zakura-core/zakura#417 (https://github.com/zakura-core/zakura/pull/417).
    pub(crate) tx: Option<Tx<Fut>>,
    pub(crate) span: tracing::Span,
    pub(super) _permit: OwnedSemaphorePermit,
}

/// Response sender
pub(crate) type Tx<Fut> = oneshot::Sender<Result<Fut, ServiceError>>;

/// Response receiver
pub(crate) type Rx<Fut> = oneshot::Receiver<Result<Fut, ServiceError>>;

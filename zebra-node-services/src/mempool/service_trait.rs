//! Trait alias for mempool-related Tower service.
//!
//! This trait provides a convenient alias for `tower::Service`
//! implementations that operate on Zebra mempool request and response types.
//!
//! - [`MempoolService`]: for services that handle unmined transaction-related requests.

use crate::{
    mempool::{Request, Response},
    service_traits::ZebraService,
};

/// Trait alias for services handling mempool requests.
pub trait MempoolService: ZebraService<Request, Response> {}

impl<T> MempoolService for T where T: ZebraService<Request, Response> {}

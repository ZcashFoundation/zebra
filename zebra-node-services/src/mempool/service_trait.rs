//! Trait alias for mempool-related Tower service.
//!
//! This trait provides a convenient alias for `tower::Service`
//! implementations that operate on Zebra mempool request and response types.
//!
//! - [`Mempool`]: for services that handle unmined transaction-related requests.

use crate::{
    mempool::{Request, Response},
    service_traits::ZebraService,
};

/// Trait alias for services handling mempool requests.
pub trait Mempool: ZebraService<Request, Response> {}

impl<T> Mempool for T where T: ZebraService<Request, Response> {}

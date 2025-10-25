//! Trait aliases for block verification Tower services.
//!
//! This trait provides a convenient alias for `tower::Service`
//! implementations that operate on Zebra block verification request and response types.
//!
//! - [`BlockVerifierService`]: for services that handle semantic block verification requests.

use crate::router::Request;
use zebra_chain::block::Hash;
use zebra_node_services::service_traits::ZebraService;

/// Trait alias for services handling semantic block verification requests.
pub trait BlockVerifierService: ZebraService<Request, Hash> {}

impl<T> BlockVerifierService for T where T: ZebraService<Request, Hash> {}

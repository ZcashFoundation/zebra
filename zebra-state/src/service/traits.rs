//! Trait aliases for state-related Tower services.
//!
//! These traits provide convenient aliases for `tower::Service`
//! implementations that operate on Zebra state request and response types.
//!
//! - [`State`]: for services that handle state-modifying requests.
//! - [`ReadState`]: for services that handle read-only state requests.

use crate::{ReadRequest, ReadResponse, Request, Response};
use zebra_node_services::service_traits::ZebraService;

/// Trait alias for services handling state-modifying requests.
pub trait State: ZebraService<Request, Response> {}

impl<T> State for T where T: ZebraService<Request, Response> {}

/// Trait alias for services handling read-only state requests.
pub trait ReadState: ZebraService<ReadRequest, ReadResponse> {}

impl<T> ReadState for T where T: ZebraService<ReadRequest, ReadResponse> {}

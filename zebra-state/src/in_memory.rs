//! A basic implementation of the zebra-state service entirely in memory
//!
//! This service is provided as an independent implementation of the
//! zebra-state service to use in verifying the correctness of `on_disk`'s
//! `Service` implementation.
use super::{Request, Response};
use std::{error, future::Future};
use tower::{buffer::Buffer, Service};

mod chains;
mod service;

/// Return's a type that implement's the `zebra_state::Service` entirely in
/// memory using `HashMaps`
pub fn init() -> impl Service<
    Request,
    Response = Response,
    Error = Error,
    Future = impl Future<Output = Result<Response, Error>>,
> + Send
       + Clone
       + 'static {
    Buffer::new(service::InMemoryState::default(), 1)
}

type Error = Box<dyn error::Error + Send + Sync + 'static>;

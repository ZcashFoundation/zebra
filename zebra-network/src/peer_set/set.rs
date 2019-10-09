use std::{
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use tokio::prelude::*;
use tower::{
    discover::{Change, Discover},
    Service,
};

use crate::{
    peer::{PeerClient, PeerError},
    protocol::internal::{Request, Response},
};

use super::PeerDiscover;

/// A [`tower::Service`] that load-balances requests across a set of connected peers.
pub struct PeerSet {
    discover: PeerDiscover,
}

impl Service<Request> for PeerSet {
    type Response = Response;
    type Error = PeerError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        unimplemented!();
    }

    fn call(&mut self, req: Request) -> Self::Future {
        unimplemented!();
    }
}

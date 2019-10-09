use std::{
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use failure::Error;
use tokio::prelude::*;
use tower::discover::{Change, Discover};

use crate::peer::PeerClient;

/// A [`tower::discover::Discover`] implementation to report new `PeerClient`s.
///
/// Because the `PeerClient` and `PeerServer` are always created together, either
/// by a `PeerConnector` or a `PeerListener`, and the `PeerServer` spawns a task
/// owned by Tokio, we only need to manage the `PeerClient` handles.
pub struct PeerDiscover {
    // add fields;
}

impl Discover for PeerDiscover {
    type Key = SocketAddr;
    type Service = PeerClient;
    type Error = Error;

    fn poll_discover(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Change<Self::Key, Self::Service>, Self::Error>> {
        // Do stuff and possibly produce new peers??

        // Change enum has insert and delete variants, but we only need to consider inserts here..
        unimplemented!();
    }
}

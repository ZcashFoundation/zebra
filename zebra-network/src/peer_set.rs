//! A peer set whose size is dynamically determined by resource constraints.

// Portions of this submodule were adapted from tower-balance,
// which is (c) 2019 Tower Contributors (MIT licensed).

mod discover;
mod set;
mod unready_service;

pub use discover::PeerDiscover;
pub use set::PeerSet;

use std::pin::Pin;

use futures::future::Future;
use tower::Service;

use crate::config::Config;
use crate::protocol::internal::{Request, Response};
use crate::BoxedStdError;
use crate::{peer::PeerConnector, timestamp_collector::TimestampCollector};

type BoxedZebraService = Box<
    dyn Service<
            Request,
            Response = Response,
            Error = BoxedStdError,
            Future = Pin<Box<dyn Future<Output = Result<Response, BoxedStdError>> + Send>>,
        > + Send
        + 'static,
>;

/// Initialize a peer set with the given `config`, forwarding peer requests to the `inbound_service`.
pub fn init<S>(config: Config, inbound_service: S) -> (BoxedZebraService, TimestampCollector)
where
    S: Service<Request, Response = Response, Error = BoxedStdError> + Clone + Send + 'static,
    S::Future: Send,
{
    use futures::{
        future,
        stream::{FuturesUnordered, StreamExt},
    };
    use tokio::net::TcpStream;
    use tower::{
        buffer::Buffer,
        discover::{Change, ServiceStream},
        ServiceExt,
    };
    use tower_load::{peak_ewma::PeakEwmaDiscover, NoInstrument};

    let tc = TimestampCollector::new();
    let pc = Buffer::new(PeerConnector::new(config.clone(), inbound_service, &tc), 1);

    // construct a stream of services XXX currently the stream is based on a
    // static stream from config.initial_peers; this should be replaced with a
    // channel that starts with initial_peers but also accetps incoming, dials
    // new, etc.
    let client_stream = PeakEwmaDiscover::new(
        ServiceStream::new(
            config
                .initial_peers
                .into_iter()
                .map(|addr| {
                    let mut pc = pc.clone();
                    async move {
                        let stream = TcpStream::connect(addr).await?;
                        pc.ready().await?;
                        let client = pc.call((stream, addr)).await?;
                        Ok::<_, BoxedStdError>(Change::Insert(addr, client))
                    }
                })
                .collect::<FuturesUnordered<_>>()
                // Discard any errored connections...
                .filter(|result| future::ready(result.is_ok())),
        ),
        config.ewma_default_rtt,
        config.ewma_decay_time,
        NoInstrument,
    );

    let peer_set = PeerSet::new(client_stream);

    (Box::new(peer_set), tc)
}

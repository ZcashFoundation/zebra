use std::sync::{Arc, Mutex};

use chrono::{TimeZone, Utc};
use futures::stream::{FuturesUnordered, Stream, StreamExt};
use tower::{Service, ServiceExt};

use crate::{types::MetaAddr, AddressBook, BoxedStdError, Request, Response};

pub(super) struct CandidateSet<S> {
    pub(super) disconnected: AddressBook,
    pub(super) gossiped: AddressBook,
    pub(super) failed: AddressBook,
    pub(super) peer_set: Arc<Mutex<AddressBook>>,
    pub(super) peer_service: S,
}

impl<S> CandidateSet<S>
where
    S: Service<Request, Response = Response, Error = BoxedStdError>,
    S::Future: Send + 'static,
{
    pub async fn update(&mut self) -> Result<(), BoxedStdError> {
        // Opportunistically crawl the network on every update call to ensure
        // we're actively fetching peers. Continue independently of whether we
        // actually receive any peers, but always ask the network for more.
        // Because requests are load-balanced across existing peers, we can make
        // multiple requests concurrently, which will be randomly assigned to
        // existing peers, but we don't make too many because update may be
        // called while the peer set is already loaded.
        let mut responses = FuturesUnordered::new();
        for _ in 0..2usize {
            self.peer_service.ready().await?;
            responses.push(self.peer_service.call(Request::GetPeers));
        }
        while let Some(rsp) = responses.next().await {
            if let Ok(Response::Peers(addrs)) = rsp {
                let addr_len = addrs.len();
                let prev_len = self.gossiped.len();
                // Filter new addresses to ensure that gossiped
                let failed = &self.failed;
                let peer_set = &self.peer_set;
                let new_addrs = addrs
                    .into_iter()
                    .filter(|meta| failed.contains_addr(&meta.addr))
                    .filter(|meta| peer_set.lock().unwrap().contains_addr(&meta.addr));
                self.gossiped.extend(new_addrs);
                trace!(
                    addr_len,
                    new_addrs = self.gossiped.len() - prev_len,
                    "got response to GetPeers"
                );
            } else {
                trace!("got error in GetPeers request");
            }
        }

        // Determine whether any known peers have recently disconnected.
        let failed = &self.failed;
        let peer_set = &self.peer_set;
        self.disconnected.extend(
            peer_set
                .lock()
                .expect("mutex must be unpoisoned")
                .disconnected_peers()
                .filter(|meta| failed.contains_addr(&meta.addr)),
        );

        Ok(())
    }

    pub fn next(&mut self) -> Option<MetaAddr> {
        self.disconnected
            .drain_oldest()
            .chain(self.gossiped.drain_newest())
            .chain(self.failed.drain_oldest())
            .next()
    }

    pub fn report_failed(&mut self, mut addr: MetaAddr) {
        addr.last_seen = Utc::now();
        self.failed.update(addr);
    }
}

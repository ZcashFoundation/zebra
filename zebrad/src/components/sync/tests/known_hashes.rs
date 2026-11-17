//! Discovery with hashes already present in state.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::Future,
    time::Duration,
    vec,
};

use color_eyre::Report;
use indexmap::IndexSet;
use tokio::{pin, select, sync::mpsc, time::timeout};

use zebra_chain::{
    block::{Hash, Height},
    chain_tip::mock::MockChainTip,
};
use zebra_network as zn;
use zebra_state as zs;
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::{
    components::sync::{ChainSync, FANOUT},
    config::ZebradConfig,
};

mod known_hash_strategy;
mod prop;

/// A mock dependency with strict request assertions.
type Mock<Req, Resp> = MockService<Req, Resp, PanicAssertion>;

/// A syncer whose discovery dependencies can be controlled independently.
struct TestScenario {
    sync: ChainSync<
        Mock<zn::Request, zn::Response>,
        Mock<zs::Request, zs::Response>,
        Mock<zs::ReadRequest, zs::ReadResponse>,
        Mock<zebra_consensus::Request, Hash>,
        MockChainTip,
    >,
    peers: Mock<zn::Request, zn::Response>,
    state: Mock<zs::Request, zs::Response>,
}

impl TestScenario {
    /// Creates a syncer without starting its background sync loop.
    fn new() -> Self {
        let peers = MockService::build().for_unit_tests();
        let state = MockService::build().for_unit_tests();
        let (tip, _) = MockChainTip::new();
        let (misbehavior, _) = mpsc::channel(1);

        let (sync, _) = ChainSync::new(
            &ZebradConfig::default(),
            Height(0),
            peers.clone(),
            MockService::build().for_unit_tests(),
            state.clone(),
            MockService::build().for_unit_tests(),
            tip,
            misbehavior,
        );

        Self { sync, peers, state }
    }

    /// Supplies one response and answers state queries with per-hash observation counts.
    async fn obtain_tips_with_hashes(
        &mut self,
        hashes: Vec<Hash>,
        known: impl FnMut(Hash, usize) -> Option<zs::KnownBlock>,
    ) -> Result<IndexSet<Hash>, Report> {
        self.obtain_tips_with_responses(vec![hashes], known).await
    }

    /// Supplies peer responses in processing order, answering unused fanout requests with empties.
    ///
    /// Waits for each response's initial state scan before releasing the next response.
    /// Polling discovery first then lets it finish merging that response before receiving another.
    ///
    /// # Panics
    ///
    /// Panics if more responses are supplied than the discovery fanout permits.
    async fn obtain_tips_with_responses(
        &mut self,
        responses: Vec<Vec<Hash>>,
        known: impl FnMut(Hash, usize) -> Option<zs::KnownBlock>,
    ) -> Result<IndexSet<Hash>, Report> {
        let operation = DiscoveryResponses::new(responses, known).run(
            self.sync.obtain_tips(),
            &mut self.peers,
            &mut self.state,
        );

        timeout(Duration::from_secs(10), operation)
            .await
            .expect("mocked discovery must finish")
    }
}

/// Coordinates ordered peer responses and per-hash state observations during discovery.
struct DiscoveryResponses<Known> {
    known: Known,
    observations: HashMap<Hash, usize>,
    responses: vec::IntoIter<Vec<Hash>>,
    pending_initial_scan: VecDeque<Hash>,
    remaining_responses: usize,
}

impl<Known> DiscoveryResponses<Known>
where
    Known: FnMut(Hash, usize) -> Option<zs::KnownBlock>,
{
    /// Creates a responder with at most one supplied response per fanout request.
    ///
    /// # Panics
    ///
    /// Panics if `responses` exceeds the discovery fanout.
    fn new(responses: Vec<Vec<Hash>>, known: Known) -> Self {
        assert!(
            responses.len() <= FANOUT,
            "responses must fit the discovery fanout"
        );

        Self {
            known,
            observations: HashMap::new(),
            responses: responses.into_iter(),
            pending_initial_scan: VecDeque::new(),
            remaining_responses: FANOUT,
        }
    }

    /// Drives discovery while servicing its mocked dependencies in processing order.
    async fn run(
        mut self,
        obtain: impl Future<Output = Result<IndexSet<Hash>, Report>>,
        peers: &mut Mock<zn::Request, zn::Response>,
        state: &mut Mock<zs::Request, zs::Response>,
    ) -> Result<IndexSet<Hash>, Report> {
        pin!(obtain);

        loop {
            select! {
                biased;
                result = &mut obtain => return result,
                request = state.expect_request_that(|_| true) => {
                    request.respond_with(|request| self.state_response(request));
                }
                request = peers.expect_request_that(|request| {
                    matches!(request, zn::Request::FindBlocks { .. })
                }), if self.remaining_responses > 0 && self.pending_initial_scan.is_empty() => {
                    request.respond(self.peer_response());
                }
            }
        }
    }

    /// Answers a state request and tracks completion of the current initial scan.
    fn state_response(&mut self, request: &zs::Request) -> zs::Response {
        match request {
            zs::Request::BlockLocator => zs::Response::BlockLocator(vec![Hash([0; 32])]),
            zs::Request::KnownBlocks(hashes) => {
                let initial_scan = !self.pending_initial_scan.is_empty();
                if initial_scan {
                    assert!(
                        hashes.iter().eq(self.pending_initial_scan.iter()),
                        "responses must be scanned in order"
                    );
                    self.pending_initial_scan.clear();
                }

                // The initial scan only uses membership up to the first unknown hash, so later
                // hashes stay unobserved, as they were when each hash was looked up separately.
                let mut found_unknown = false;
                let locations = hashes
                    .iter()
                    .map(|hash| {
                        if initial_scan && found_unknown {
                            return None;
                        }
                        let location = self.known_block(*hash);
                        found_unknown |= matches!(location, None | Some(zs::KnownBlock::Queue));
                        location
                    })
                    .collect();

                zs::Response::KnownBlocks(locations)
            }
            request => panic!("unexpected state request: {request:?}"),
        }
    }

    /// Supplies the next peer response and starts tracking its initial state scan.
    fn peer_response(&mut self) -> zn::Response {
        self.remaining_responses -= 1;
        let hashes = self.responses.next().unwrap_or_default();
        self.pending_initial_scan = VecDeque::from(hashes.clone());

        zn::Response::BlockHashes(hashes)
    }

    /// Returns membership for the next observation of `hash`.
    fn known_block(&mut self, hash: Hash) -> Option<zs::KnownBlock> {
        let count = self.observations.entry(hash).or_insert(0);
        *count += 1;

        (self.known)(hash, *count)
    }
}

/// Checks that precisely the expected hashes were scheduled for download.
async fn assert_downloaded_hashes(test: &mut TestScenario, hashes: impl IntoIterator<Item = Hash>) {
    let mut remaining_hashes = HashSet::<Hash>::from_iter(hashes);
    assert_eq!(test.sync.downloads.in_flight(), remaining_hashes.len());

    for _ in 0..remaining_hashes.len() {
        test.peers
            .expect_request_that(|request| {
                matches!(request, zn::Request::BlockDownload { hash, advertiser: None }
                    if remaining_hashes.remove(hash))
            })
            .await
            .respond(Err(zn::BoxError::from(
                "download not needed for discovery test",
            )));
    }

    assert!(
        remaining_hashes.is_empty(),
        "missing downloads: {remaining_hashes:?}"
    );
}

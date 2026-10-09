//! Block propagation tests with controlled tip, submission, and network channels.

use std::sync::Arc;

use tokio::{sync::oneshot, task::JoinHandle, time::Instant};
use zebra_chain::{
    block::Block, parameters::Network::Mainnet, serialization::ZcashDeserializeInto,
};
use zebra_state::{ChainTipBlock, ChainTipSender, CheckpointVerifiedBlock};

use super::*;
use crate::components::sync::RecentSyncLengths;

type Broadcast = (zn::Request, oneshot::Sender<zn::Response>);

struct Setup {
    tip_sender: ChainTipSender,
    recent_syncs: RecentSyncLengths,
    submissions: mpsc::Sender<(block::Hash, block::Height)>,
    requests: mpsc::UnboundedReceiver<Broadcast>,
    task: JoinHandle<Result<(), BlockGossipError>>,
}

impl Setup {
    fn new(close_to_tip: bool, mined_channel: bool) -> Self {
        let (tip_sender, _, tip_changes) = ChainTipSender::new(None::<ChainTipBlock>, &Mainnet);
        let (sync_status, mut recent_syncs) = SyncStatus::new();
        if close_to_tip {
            SyncStatus::sync_close_to_tip(&mut recent_syncs);
        }
        let (submissions, receiver) = mpsc::channel(8);
        let (requests_sender, requests) = mpsc::unbounded_channel();
        let network = tower::service_fn(move |request| {
            let (sender, receiver) = oneshot::channel();
            requests_sender.send((request, sender)).unwrap();
            async move { Ok::<_, BoxError>(receiver.await.unwrap()) }
        });
        let task = tokio::spawn(gossip_best_tip_block_hashes(
            sync_status,
            tip_changes,
            network,
            mined_channel.then_some(receiver),
        ));
        Self {
            tip_sender,
            recent_syncs,
            submissions,
            requests,
            task,
        }
    }

    fn commit(&mut self, block: Arc<Block>) {
        self.tip_sender
            .set_finalized_tip(Some(CheckpointVerifiedBlock::from(block).into()));
    }

    async fn submit(&self, block: &Block) {
        self.submissions
            .send((block.hash(), block.coinbase_height().unwrap()))
            .await
            .unwrap();
    }

    async fn expect(&mut self, request: zn::Request) -> oneshot::Sender<zn::Response> {
        let (actual, response) = self.requests.recv().await.unwrap();
        assert_eq!(actual, request);
        response
    }

    async fn assert_no_requests(&mut self) {
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_millis(1)).await;
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        assert!(self.requests.try_recv().is_err());
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn block(height: u32) -> Arc<Block> {
    let bytes = zebra_test::vectors::MAINNET_BLOCKS[&height];
    bytes.zcash_deserialize_into().unwrap()
}

#[tokio::test(start_paused = true)]
async fn committed_blocks_propagate_without_a_gossip_delay() {
    let mut setup = Setup::new(true, false);
    for height in 1..=3 {
        let block = block(height);
        let start = Instant::now();
        setup.commit(block.clone());
        setup
            .expect(zn::Request::AdvertiseBlock(block.hash(), None))
            .await
            .send(zn::Response::Nil)
            .unwrap();
        assert_eq!(Instant::now(), start);
    }
}

#[tokio::test(start_paused = true)]
async fn simultaneous_submission_and_tip_are_broadcast_exactly_once() {
    let mut setup = Setup::new(true, true);
    let block = block(1);
    setup.commit(block.clone());
    setup.submit(&block).await;
    let start = Instant::now();
    setup
        .expect(zn::Request::AdvertiseBlockToAll(block.hash()))
        .await
        .send(zn::Response::Nil)
        .unwrap();
    assert_eq!(Instant::now(), start);
    tokio::time::advance(Duration::from_secs(30)).await;
    setup.assert_no_requests().await;
}

#[tokio::test(start_paused = true)]
async fn submission_before_tip_and_duplicate_notification_are_not_rebroadcast() {
    let mut setup = Setup::new(true, true);
    let block = block(1);
    setup.submit(&block).await;
    setup
        .expect(zn::Request::AdvertiseBlockToAll(block.hash()))
        .await
        .send(zn::Response::Nil)
        .unwrap();
    setup.submit(&block).await;
    setup.commit(block);
    tokio::time::advance(Duration::from_secs(30)).await;
    setup.assert_no_requests().await;
}

#[tokio::test(start_paused = true)]
async fn ordinary_gossip_does_not_suppress_later_all_peer_coverage() {
    let mut setup = Setup::new(true, true);
    let block = block(1);
    setup.commit(block.clone());
    setup
        .expect(zn::Request::AdvertiseBlock(block.hash(), None))
        .await
        .send(zn::Response::Nil)
        .unwrap();
    setup.submit(&block).await;
    setup
        .expect(zn::Request::AdvertiseBlockToAll(block.hash()))
        .await
        .send(zn::Response::Nil)
        .unwrap();
    setup.submit(&block).await;
    setup.assert_no_requests().await;
}

#[tokio::test(start_paused = true)]
async fn slow_broadcasts_bound_work_and_coalesce_to_latest_committed_tip() {
    let mut setup = Setup::new(true, false);
    let first = block(1);
    setup.commit(first.clone());
    let response = setup
        .expect(zn::Request::AdvertiseBlock(first.hash(), None))
        .await;
    setup.commit(block(2));
    let latest = block(3);
    setup.commit(latest.clone());
    setup.assert_no_requests().await;
    response.send(zn::Response::Nil).unwrap();
    setup
        .expect(zn::Request::AdvertiseBlock(latest.hash(), None))
        .await
        .send(zn::Response::Nil)
        .unwrap();
    setup.assert_no_requests().await;
}

#[tokio::test(start_paused = true)]
async fn slow_responses_keep_draining_mined_notifications() {
    let mut setup = Setup::new(false, true);
    let first = block(1);
    setup.submit(&first).await;
    let stalled = setup
        .expect(zn::Request::AdvertiseBlockToAll(first.hash()))
        .await;
    let latest = block(2);
    for _ in 0..32 {
        setup
            .submissions
            .try_send((latest.hash(), latest.coinbase_height().unwrap()))
            .expect("gossip drains the bounded channel while a response is pending");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    setup.assert_no_requests().await;
    stalled.send(zn::Response::Nil).unwrap();
    setup
        .expect(zn::Request::AdvertiseBlockToAll(latest.hash()))
        .await
        .send(zn::Response::Nil)
        .unwrap();
    setup.assert_no_requests().await;
}

#[tokio::test(start_paused = true)]
async fn syncing_coalesces_commits_until_close_to_tip() {
    let mut setup = Setup::new(false, false);
    setup.commit(block(1));
    setup.assert_no_requests().await;
    let latest = block(2);
    setup.commit(latest.clone());
    SyncStatus::sync_close_to_tip(&mut setup.recent_syncs);
    setup
        .expect(zn::Request::AdvertiseBlock(latest.hash(), None))
        .await
        .send(zn::Response::Nil)
        .unwrap();
    setup.assert_no_requests().await;
}

#[tokio::test(start_paused = true)]
async fn timed_out_mined_broadcast_keeps_committed_tip_fallback() {
    let mut setup = Setup::new(true, true);
    let block = block(1);
    setup.commit(block.clone());
    setup.submit(&block).await;
    let stalled = setup
        .expect(zn::Request::AdvertiseBlockToAll(block.hash()))
        .await;
    tokio::time::advance(TIPS_RESPONSE_TIMEOUT).await;
    setup
        .expect(zn::Request::AdvertiseBlock(block.hash(), None))
        .await
        .send(zn::Response::Nil)
        .unwrap();
    drop(stalled);
    setup.assert_no_requests().await;
}

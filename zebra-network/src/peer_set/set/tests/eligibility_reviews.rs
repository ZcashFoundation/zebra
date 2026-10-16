//! Consumer-visible eligibility and scheduling regressions from the auxiliary review.

use futures::FutureExt as _;

use super::*;

#[test]
fn queued_block_ignores_banned_or_outdated_busy_peer() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();

    runtime.block_on(async {
        for (ban, fallback_missing) in [(true, false), (false, false), (true, true), (false, true)]
        {
            let serving_addr: PeerSocketAddr = "127.0.0.1:1".parse().unwrap();
            let fallback_addr: PeerSocketAddr = "127.0.0.2:1".parse().unwrap();
            let (serving, _serving_handle) = ClientTestHarness::build()
                .with_version(Version::min_remote_for_height(&Network::Mainnet, None))
                .with_services(PeerServices::NODE_NETWORK)
                .finish();
            let (fallback, mut fallback_handle) = ClientTestHarness::build()
                .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
                .with_services(PeerServices::empty())
                .finish();
            let discovery = stream::iter([
                Ok::<_, BoxError>(Change::Insert(serving_addr, serving.into())),
                Ok(Change::Insert(fallback_addr, fallback.into())),
            ])
            .chain(stream::pending());
            let (minimum, best_tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
            let (mut peers, mut peer_guard) = PeerSetBuilder::new()
                .with_discover(discovery)
                .with_minimum_peer_version(minimum)
                .build();
            let (bans_tx, bans_rx) = watch::channel(crate::BanList::default());
            peers.bans_receiver = bans_rx;
            let _busy = make_serving_peer_busy(&mut peers, serving_addr).await;
            if fallback_missing {
                send_inventory(
                    &mut peer_guard,
                    InventoryStatus::new_missing(
                        InventoryHash::Block(block::Hash([2; 32])),
                        fallback_addr,
                    ),
                );
            }
            let download = peers.ready().await.unwrap().call(block_request(2));
            assert_eq!(peers.queued_inv_requests.len(), 1);

            if ban {
                bans_tx.send_modify(|bans| {
                    bans.ban(serving_addr.ip());
                });
            } else {
                best_tip.send_best_tip_height(
                    NetworkUpgrade::Nu6_3.activation_height(&Network::Mainnet),
                );
            }
            timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT / 4, peers.ready())
                .await
                .expect("eligibility changes must reconsider the existing queue promptly")
                .expect("fallback remains ready");
            assert!(peers.queued_inv_requests.is_empty());
            if fallback_missing {
                assert_not_found_registry(
                    download
                        .now_or_never()
                        .expect("no eligible candidate must refuse the queued request immediately"),
                );
                assert_eq!(received_request(&mut fallback_handle), None);
            } else {
                assert_eq!(
                    received_request(&mut fallback_handle),
                    Some(block_request(2)),
                    "removing a busy candidate must reroute the queued request (ban={ban})"
                );
            }
        }
    });
}

/// Drain a real client channel when scheduling wakes it. This deterministically models the
/// connection task dequeuing the first find between the peer set's two readiness passes.
struct DrainOnWake {
    outer: std::task::Waker,
    client: std::sync::Arc<tokio::sync::Mutex<ClientTestHarness>>,
    received: std::sync::Arc<tokio::sync::Mutex<Vec<crate::peer::ReceivedClientRequest>>>,
}

impl std::task::Wake for DrainOnWake {
    fn wake(self: std::sync::Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &std::sync::Arc<Self>) {
        if let Ok(mut client) = self.client.try_lock() {
            while let Some(request) = client.take_request() {
                self.received
                    .try_lock()
                    .expect("test request log is not locked across awaits")
                    .push(request);
            }
        }
        self.outer.wake_by_ref();
    }
}

struct DrainBetweenPolls<S> {
    inner: S,
    client: std::sync::Arc<tokio::sync::Mutex<ClientTestHarness>>,
    received: std::sync::Arc<tokio::sync::Mutex<Vec<crate::peer::ReceivedClientRequest>>>,
}

impl<S: Service<Request>> Service<Request> for DrainBetweenPolls<S> {
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        let waker = std::task::Waker::from(std::sync::Arc::new(DrainOnWake {
            outer: cx.waker().clone(),
            client: self.client.clone(),
            received: self.received.clone(),
        }));
        self.inner
            .poll_ready(&mut std::task::Context::from_waker(&waker))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        self.inner.call(request)
    }
}

#[test]
fn queued_find_priority_survives_second_readiness_pass() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();

    runtime.block_on(async {
        let (mut peers, _guard, addrs, mut handles, _) =
            peer_set_with_services(&SERVING_AND_NON_SERVING);
        let _busy = make_serving_peer_busy(&mut peers, addrs[0]).await;
        let find = find_requests_and_responses()[0].0.clone();
        let mut pending = Vec::new();
        for _ in 0..2 {
            pending.push(
                peers
                    .ready()
                    .await
                    .expect("fallback is ready")
                    .call(find.clone()),
            );
        }
        // Occupy the non-serving peer so that dispatching the first find forces a second pass.
        for _ in 0..2 {
            pending.push(
                peers
                    .ready()
                    .await
                    .expect("fallback has channel capacity")
                    .call(Request::Peers),
            );
        }
        assert_eq!(received_request(&mut handles[0]), Some(block_request(1)));
        assert_eq!(received_request(&mut handles[0]), Some(block_request(9)));
        let client = std::sync::Arc::new(tokio::sync::Mutex::new(handles.remove(0)));
        let received = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let mut buffered = tower::buffer::Buffer::new(
            DrainBetweenPolls {
                inner: peers,
                client: client.clone(),
                received: received.clone(),
            },
            3,
        );
        let ordinary_request = block_request(4);
        let ordinary = buffered
            .ready()
            .await
            .expect("buffer accepts ordinary work")
            .call(ordinary_request.clone());
        timeout(Duration::from_secs(1), async {
            loop {
                // The wake callback injects capacity between readiness passes, but is not a
                // request observer: AtomicWaker consumes its registration on the first wake.
                // Independently drain the real client, as a connection task would.
                let complete = {
                    let mut client = client
                        .try_lock()
                        .expect("test client is not locked across awaits");
                    let mut received = received
                        .try_lock()
                        .expect("test request log is not locked across awaits");
                    while let Some(request) = client.take_request() {
                        received.push(request);
                    }
                    for (actual, expected) in received.iter().zip([&find, &find, &ordinary_request])
                    {
                        assert_eq!(
                            actual.request(),
                            expected,
                            "the buffer's held ordinary request must follow both queued finds"
                        );
                    }
                    received.len() == 3
                };
                if complete {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("queued finds and held ordinary work must dispatch without other traffic");
        // Cancel callers before dropping the real response senders retained in the log.
        drop((pending, ordinary));
    });
}

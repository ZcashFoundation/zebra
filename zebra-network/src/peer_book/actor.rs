//! The peer book actor: a single blocking thread that owns the address book.
//!
//! The actor is the address book's only owner. It applies
//! [`MetaAddrChange`]s and answers [`PeerBookHandle`](super::PeerBookHandle)
//! calls in channel order on one dedicated blocking thread, so book access
//! needs no locks at all, and it maintains the bans and recently-live watch
//! snapshots for lock-free readers.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::Utc;
use indexmap::IndexMap;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tracing::Span;

use crate::{
    address_book_updater::AllAddressBookUpdaterSendersClosed,
    meta_addr::{MetaAddr, MetaAddrChange},
    peer_book::{
        handle::{Call, Message},
        misbehavior::{BanKey, MisbehaviorStore},
        transports::TransportTable,
        PeerBookRequest, PeerBookResponse,
    },
    AddressBook, AddressBookPeers, BoxError,
};

/// The minimum time between refreshes of the recently-live watch snapshot.
///
/// Readers filter the snapshot by their own current time, so a stale
/// superset only delays the appearance of *newly* live peers: peers that
/// went stale are filtered out immediately. The actor also refreshes
/// whenever its message channel drains, so the snapshot is at most this
/// stale only during sustained change bursts.
const RECENTLY_LIVE_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// Spawns the peer book actor, which owns `address_book`.
///
/// The actor exits with an error when every sender and handle is dropped,
/// and keeps the `bans` and `recently_live` watch channels up to date while
/// it runs.
pub(crate) fn spawn_actor(
    mut address_book: AddressBook,
    mut messages_rx: mpsc::Receiver<Message>,
    bans_sender: watch::Sender<Arc<IndexMap<BanKey, Instant>>>,
    recently_live_sender: watch::Sender<Arc<Vec<MetaAddr>>>,
    #[cfg(feature = "progress-bar")] mut address_info: watch::Receiver<
        crate::address_book::AddressMetrics,
    >,
) -> JoinHandle<Result<(), BoxError>> {
    let actor = move || {
        info!("starting the peer book actor");

        #[cfg(feature = "progress-bar")]
        let (address_bar, never_bar, failed_bar) = {
            let address_bar = howudoin::new_root().label("Known Peers");
            let never_bar =
                howudoin::new_with_parent(address_bar.id()).label("Never Attempted Peers");
            let failed_bar = howudoin::new_with_parent(never_bar.id()).label("Failed Peers");

            (address_bar, never_bar, failed_bar)
        };

        let mut misbehavior = MisbehaviorStore::default();
        let mut gossip_buckets = super::buckets::GossipBuckets::new();
        let mut transports = super::transports::TransportTable::default();
        let mut get_addr_cache: Option<(Instant, Vec<MetaAddr>)> = None;
        let mut last_live_refresh = Instant::now();
        let mut live_changed = false;

        while let Some(message) = messages_rx.blocking_recv() {
            match message {
                Message::Change(change) => {
                    trace!(?change, "got address book change");

                    let ban_key = BanKey::from(change.addr().ip());

                    if let MetaAddrChange::UpdateMisbehavior {
                        score_increment, ..
                    } = &change
                    {
                        // The store is authoritative for scores and bans:
                        // it is keyed by address, persists across
                        // connections for about the ban duration, and book
                        // churn can never launder it. The change is still
                        // applied to the book entry below, as a display
                        // mirror for peer diagnostics.
                        if misbehavior.record(ban_key, *score_increment, Instant::now()) {
                            info!(?ban_key, "banning misbehaving peer");

                            address_book.remove_if_key_matches(|ip| BanKey::from(ip) == ban_key);
                            gossip_buckets.remove_if(|addr| BanKey::from(addr.ip()) == ban_key);
                            transports.remove_if(|addr| BanKey::from(addr.ip()) == ban_key);
                            let _ = bans_sender.send(misbehavior.bans_snapshot());

                            continue;
                        }
                    } else if misbehavior.is_banned(&ban_key) {
                        // Ignore changes for banned peers, so they cannot
                        // re-enter the book while the ban lasts.
                        continue;
                    }

                    // # Security
                    //
                    // Gossiped addresses are unauthenticated relay data:
                    // new ones are admitted through the secret-keyed
                    // buckets, which bound the book share of any address
                    // group and of gossip as a whole (eclipse resistance).
                    if let MetaAddrChange::NewGossiped { addr, .. } = &change {
                        let addr = *addr;
                        if address_book.get(addr).is_none() {
                            let victim = gossip_buckets.admit(addr, |entry| {
                                address_book.get(entry).is_some_and(|meta| {
                                    meta.last_connection_state
                                        == crate::PeerAddrState::NeverAttemptedGossiped
                                })
                            });
                            if let Some(victim) = victim {
                                address_book.remove(victim);
                            }
                        }
                    }

                    address_book.update(change);
                    live_changed = true;
                }

                Message::Transport {
                    addr,
                    transport,
                    reachable,
                } => {
                    if reachable {
                        transports.record_reachable(addr, transport);
                    } else {
                        transports.record_unreachable(addr, transport, Instant::now());
                    }
                }

                Message::Call(Call { request, reply }) => {
                    let response = answer_call(
                        &mut address_book,
                        &mut misbehavior,
                        &mut transports,
                        &mut get_addr_cache,
                        request,
                    );

                    // The caller may have given up on the request: reply
                    // errors are not this actor's fault.
                    let _ = reply.send(Ok(response));
                }
            }

            // Refresh the recently-live snapshot when the message channel
            // drains, or at most once per refresh interval during bursts.
            // Read-only calls cannot change liveness, so they never trigger
            // a book scan.
            if live_changed
                && (messages_rx.is_empty()
                    || last_live_refresh.elapsed() >= RECENTLY_LIVE_REFRESH_INTERVAL)
            {
                let live = address_book.recently_live_peers(Utc::now());

                let _ = recently_live_sender.send(Arc::new(live));
                last_live_refresh = Instant::now();
                live_changed = false;
            }

            #[cfg(feature = "progress-bar")]
            if matches!(howudoin::cancelled(), Some(true)) {
                address_bar.close();
                never_bar.close();
                failed_bar.close();
            } else if address_info.has_changed()? {
                // We don't track:
                // - attempt pending because it's always small
                // - responded because it's the remaining attempted-but-not-failed peers
                // - recently live because it's similar to the connected peer counts

                let address_info = *address_info.borrow_and_update();

                address_bar
                    .set_pos(u64::try_from(address_info.num_addresses).expect("fits in u64"));

                never_bar.set_pos(
                    u64::try_from(address_info.never_attempted_gossiped).expect("fits in u64"),
                );

                failed_bar.set_pos(u64::try_from(address_info.failed).expect("fits in u64"));
            }
        }

        #[cfg(feature = "progress-bar")]
        {
            address_bar.close();
            never_bar.close();
            failed_bar.close();
        }

        let error = Err(AllAddressBookUpdaterSendersClosed.into());
        info!(?error, "stopping the peer book actor");
        error
    };

    // The actor accesses the address book on its own dedicated blocking
    // thread, so async tasks never block on book operations (#1976).
    let span = Span::current();
    tokio::task::spawn_blocking(move || span.in_scope(actor))
}

/// How long one sanitized `get-addr` response snapshot is served before a
/// fresh sample is drawn, before jitter.
///
/// Serving a cached snapshot stops repeated address requests from
/// enumerating the book incrementally: every requester observes the same
/// sample for the interval. These are the zcashd reference values, cited by
/// the draft ZIP via ZIP 204.
const GET_ADDR_CACHE_BASE_INTERVAL: Duration = Duration::from_secs(21 * 60 * 60);

/// The maximum random jitter added to the `get-addr` snapshot interval at
/// each rotation.
const GET_ADDR_CACHE_JITTER: Duration = Duration::from_secs(6 * 60 * 60);

/// Answers one [`PeerBookHandle`](super::PeerBookHandle) request against the
/// book.
fn answer_call(
    address_book: &mut AddressBook,
    misbehavior: &mut MisbehaviorStore,
    transports: &mut TransportTable,
    get_addr_cache: &mut Option<(Instant, Vec<MetaAddr>)>,
    request: PeerBookRequest,
) -> PeerBookResponse {
    match request {
        PeerBookRequest::SanitizedAddrs => {
            // # Security
            //
            // One sanitized snapshot is served for the whole cache
            // interval, so repeated requests cannot enumerate the book
            // incrementally. An empty snapshot is never pinned: the first
            // requests after startup would otherwise stay unanswerable for
            // the interval while the book fills.
            let stale = get_addr_cache
                .as_ref()
                .is_none_or(|(deadline, addrs)| Instant::now() >= *deadline || addrs.is_empty());
            if stale {
                let addrs = address_book.fresh_get_addr_response();
                let jitter = GET_ADDR_CACHE_JITTER.mul_f64(rand::random::<f64>());
                let deadline = Instant::now() + GET_ADDR_CACHE_BASE_INTERVAL + jitter;
                *get_addr_cache = Some((deadline, addrs));
            }

            let (_, addrs) = get_addr_cache.as_ref().expect("the cache was just filled");
            PeerBookResponse::Addrs(addrs.clone())
        }
        PeerBookRequest::CacheSnapshot => {
            PeerBookResponse::Addrs(address_book.cacheable(Utc::now()))
        }
        PeerBookRequest::SelectCandidates { max } => {
            // The pick and the attempt mark happen in the same actor turn,
            // so concurrent selections cannot return the same peer.
            let instant_now = Instant::now();
            let chrono_now = Utc::now();

            let mut candidates = Vec::new();
            for _ in 0..max {
                let Some(next_peer) = address_book
                    .reconnection_peers(instant_now, chrono_now)
                    .next()
                else {
                    break;
                };

                let change = MetaAddr::new_reconnect(next_peer.addr);
                if let Some(marked) = address_book.update(change) {
                    // The dialer needs to know which transports this peer
                    // accepts, so it can reach version 2 peers it learned
                    // about over the legacy network.
                    let transports = transports.dialable(&marked.addr, instant_now);
                    candidates.push((marked, transports));
                }
            }

            PeerBookResponse::Candidates(candidates)
        }
        PeerBookRequest::ReadyCandidateCount => {
            let instant_now = Instant::now();
            let chrono_now = Utc::now();

            PeerBookResponse::ReadyCandidateCount(
                address_book
                    .reconnection_peers(instant_now, chrono_now)
                    .count(),
            )
        }
        PeerBookRequest::GossipedAddrs { addrs } => {
            // # Security
            //
            // Validation is the actor's job, so no gossiped address reaches
            // the book without it, and banned peers cannot re-enter the
            // book through gossip.
            let addrs =
                super::intake::validate_addrs(addrs, zebra_chain::serialization::DateTime32::now());

            let changes: Vec<MetaAddrChange> = addrs
                .filter(|addr| !misbehavior.is_banned(&BanKey::from(addr.addr.ip())))
                .map(MetaAddr::new_gossiped_change)
                .map(|change| change.expect("gossiped peers always have services set"))
                .collect();

            address_book.extend(changes);

            PeerBookResponse::Done
        }
        #[cfg(any(test, feature = "proptest-impl"))]
        PeerBookRequest::TestSetLocalListener(addr) => {
            address_book.set_local_listener(addr);

            PeerBookResponse::Done
        }
        #[cfg(any(test, feature = "proptest-impl"))]
        PeerBookRequest::TestPeerEntry(addr) => PeerBookResponse::PeerEntry(address_book.get(addr)),
    }
}

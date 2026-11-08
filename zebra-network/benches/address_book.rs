//! Address-book costs only: no sockets, node, timers, peer-set scheduler, or consensus work.
//! Uses production public APIs, including their metric scans, with tracing/metrics exporters off.
//! 6,500 advertised addresses exercise the production 5,000-entry cap, not 6,500 resident peers.
//! Up to 750 entries are live outbound peers; other entries remain discovered candidates.
use std::{
    hint::black_box,
    net::{Ipv6Addr, SocketAddr},
    time::Duration,
};

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput};
use zebra_chain::{parameters::Network, serialization::DateTime32};
use zebra_network::{
    constants::{
        CURRENT_NETWORK_PROTOCOL_VERSION, MAX_ADDRS_IN_ADDRESS_BOOK, MAX_PEER_MISBEHAVIOR_SCORE,
    },
    types::{MetaAddr, PeerServices},
    AddressBook, PeerAddrState, PeerSocketAddr,
};

fn addr(index: usize) -> PeerSocketAddr {
    // Sixteen independently addressed peers per global-unicast IPv6 /64.
    let group = u16::try_from(index / 16 + 1).expect("benchmark groups fit in u16");
    let host = u16::try_from(index % 16 + 1).expect("sixteen hosts fit in u16");
    SocketAddr::from((
        Ipv6Addr::new(0x2606, 0x4700, group, 1, 0, 0, 0, host),
        18233,
    ))
    .into()
}

fn book(advertised: usize) -> AddressBook {
    let mut book = AddressBook::new(
        "127.0.0.1:18233".parse().unwrap(),
        &Network::new_regtest(Default::default()),
        1,
        tracing::Span::none(),
    );
    let now = DateTime32::now();
    book.extend((0..advertised).map(|i| {
        MetaAddr::new_gossiped_meta_addr(addr(i), PeerServices::NODE_NETWORK, now)
            .new_gossiped_change()
            .unwrap()
    }));
    assert_eq!(book.len(), advertised.min(MAX_ADDRS_IN_ADDRESS_BOOK));
    let live = 750.min(book.len() / 2);
    book.extend((0..live).map(|i| {
        MetaAddr::new_connected(
            addr(i),
            &PeerServices::NODE_NETWORK,
            false,
            String::new(),
            CURRENT_NETWORK_PROTOCOL_VERSION,
        )
    }));
    assert_eq!(book.state_peers(PeerAddrState::Responded).count(), live);
    book
}

fn benchmark(c: &mut Criterion) {
    for advertised in [200, 750, 1250, 2000, 5000, 6500] {
        let baseline = book(advertised);
        let peers: Vec<_> = baseline
            .state_peers(PeerAddrState::NeverAttemptedGossiped)
            .map(|peer| peer.addr())
            .collect();
        let connect = |peer| {
            MetaAddr::new_connected(
                peer,
                &PeerServices::NODE_NETWORK,
                false,
                String::new(),
                CURRENT_NETWORK_PROTOCOL_VERSION,
            )
        };

        // Runnable correctness checks also run under Criterion's --test mode.
        let mut checked = baseline.clone();
        assert!(checked.update(connect(peers[0])).is_some());
        assert_eq!(
            checked.get(peers[0]).unwrap().last_connection_state(),
            PeerAddrState::Responded
        );
        let mut checked = baseline.clone();
        checked.update(MetaAddr::new_misbehavior(
            peers[0],
            MAX_PEER_MISBEHAVIOR_SCORE,
            "bench",
        ));
        assert_eq!(checked.len(), baseline.len() - 16.min(baseline.len()));
        assert!(checked.get(peers[0]).is_none());
        assert_eq!(
            checked.misbehavior_score(peers[0]),
            MAX_PEER_MISBEHAVIOR_SCORE
        );
        let mut checked = baseline.clone();
        checked.extend([
            MetaAddr::new_reconnect(peers[0]),
            connect(peers[0]),
            MetaAddr::new_shutdown(peers[0]),
        ]);
        assert_eq!(
            checked.get(peers[0]).unwrap().last_connection_state(),
            PeerAddrState::Failed
        );

        let mut group = c.benchmark_group(format!(
            "advertised_{advertised}_resident_{}",
            baseline.len()
        ));
        group.throughput(Throughput::Elements(1));
        group.bench_function("public_update_connected", |b| {
            b.iter_batched_ref(
                || baseline.clone(),
                |book| {
                    black_box(book.update(connect(peers[0])));
                },
                BatchSize::PerIteration,
            )
        });
        let batch = 375.min(peers.len());
        group.throughput(Throughput::Elements(u64::try_from(batch).unwrap()));
        group.bench_function(BenchmarkId::new("extend_connected", batch), |b| {
            b.iter_batched_ref(
                || baseline.clone(),
                |book| book.extend(peers[..batch].iter().copied().map(connect)),
                BatchSize::PerIteration,
            )
        });
        let cycles = 750.min(peers.len());
        group.throughput(Throughput::Elements(u64::try_from(cycles).unwrap()));
        group.bench_function(
            BenchmarkId::new("address_state_churn_cycles", cycles),
            |b| {
                b.iter_batched_ref(
                    || baseline.clone(),
                    |book| {
                        book.extend(peers[..cycles].iter().copied().flat_map(|peer| {
                            [
                                MetaAddr::new_reconnect(peer),
                                connect(peer),
                                MetaAddr::new_shutdown(peer),
                            ]
                        }))
                    },
                    BatchSize::PerIteration,
                )
            },
        );
        group.throughput(Throughput::Elements(16));
        group.bench_function("ipv6_group_ban_16", |b| {
            b.iter_batched_ref(
                || baseline.clone(),
                |book| {
                    black_box(book.update(MetaAddr::new_misbehavior(
                        peers[0],
                        MAX_PEER_MISBEHAVIOR_SCORE,
                        "bench",
                    )));
                },
                BatchSize::PerIteration,
            )
        });
        if advertised == 5000 {
            let additions: Vec<_> = (0..375)
                .map(|i| {
                    // Interleave fresh addresses near the front, rather than self-evicting at the tail.
                    let mut fresh = *addr(i);
                    fresh.set_port(18234);
                    MetaAddr::new_gossiped_meta_addr(
                        fresh.into(),
                        PeerServices::NODE_NETWORK,
                        DateTime32::now(),
                    )
                })
                .collect();
            let mut checked = baseline.clone();
            checked.extend(
                additions
                    .iter()
                    .cloned()
                    .map(|peer| peer.new_gossiped_change().unwrap()),
            );
            assert_eq!(checked.len(), MAX_ADDRS_IN_ADDRESS_BOOK);
            assert!(checked.get(additions[0].addr()).is_some());
            assert!(checked.get(addr(4999)).is_none());
            group.throughput(Throughput::Elements(375));
            group.bench_function("full_book_insert_evict_375", |b| {
                b.iter_batched_ref(
                    || baseline.clone(),
                    |book| {
                        book.extend(
                            additions
                                .iter()
                                .cloned()
                                .map(|peer| peer.new_gossiped_change().unwrap()),
                        )
                    },
                    BatchSize::PerIteration,
                )
            });
        }
        group.finish();
    }
}

fn main() {
    let mut criterion = Criterion::default()
        .sample_size(30)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3))
        .without_plots()
        .configure_from_args();
    benchmark(&mut criterion);
    criterion.final_summary();
}

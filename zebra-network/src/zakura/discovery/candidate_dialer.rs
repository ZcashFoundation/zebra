//! Background dialer that connects out to peers learned through discovery.
//!
//! Bootstrap dials are owned by [`super::dialer`]; this dialer pulls dial
//! candidates from the discovery book, reserves per-IP capacity so it never
//! exceeds the connection caps, and dials them under the same admission control
//! as bootstrap peers. Discovery success is the peer appearing in the supervisor
//! registration watch, not the dial future completing.

use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    time::Duration,
};

use iroh::{NodeAddr, NodeId};
use tokio::task::JoinSet;
use tracing::debug;

use super::dialer::native_bootstrap_dial;
use super::protocol::{ZakuraDiscoveryDialCandidate, ZakuraDiscoveryHandle};
use super::redial::ZAKURA_REDIAL_HEALTHY_CONNECTION;
use crate::zakura::{
    trace::{discovery_trace as d_trace, peer_label, DISCOVERY_TABLE},
    ZakuraEndpoint, ZakuraHandlerError, ZakuraLocalLimits, ZakuraPeerId,
};

/// How often the discovery dialer wakes to look for new candidates.
const ZAKURA_DISCOVERY_DIAL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum DiscoveryDialResult {
    Registered,
    ShortLivedRegistered,
    Failed,
    LocalResourceLimit,
}

impl DiscoveryDialResult {
    fn label(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::ShortLivedRegistered => "short_lived_registered",
            Self::Failed => "failed",
            Self::LocalResourceLimit => "local_resource_limit",
        }
    }
}

#[derive(Debug)]
struct DiscoveryDialWorkerResult {
    node_id: NodeId,
    reserved_ips: Vec<IpAddr>,
    result: DiscoveryDialResult,
}

/// Spawn the long-lived discovery candidate dialer for `endpoint`.
///
/// Returns the task handle so the caller can track it under the endpoint
/// shutdown owner; the loop also observes the endpoint shutdown token directly.
pub(crate) fn spawn_native_discovery_dialer(
    endpoint: ZakuraEndpoint,
    discovery: ZakuraDiscoveryHandle,
    limits: ZakuraLocalLimits,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_native_discovery_dialer(endpoint, discovery, limits))
}

/// Seed the discovery book with the configured bootstrap peers as trusted static
/// dial candidates, so the candidate dialer maintains them even before any peer
/// gossips a signed record for them.
pub(crate) async fn insert_static_bootstrap_candidates(
    discovery: &ZakuraDiscoveryHandle,
    bootstrap_peers: &[String],
) {
    for entry in bootstrap_peers {
        match super::dialer::parse_bootstrap_peer(entry) {
            Ok(node_addr) => {
                if let Err(error) = discovery.insert_static_candidate(node_addr).await {
                    debug!(%entry, ?error, "ignoring un-insertable Zakura bootstrap candidate");
                }
            }
            Err(error) => {
                debug!(%entry, ?error, "ignoring malformed Zakura bootstrap peer");
            }
        }
    }
}

pub(crate) async fn run_native_discovery_dialer(
    endpoint: ZakuraEndpoint,
    discovery: ZakuraDiscoveryHandle,
    limits: ZakuraLocalLimits,
) {
    let shutdown = endpoint.background_shutdown_token();
    let trace = endpoint.trace();
    let mut registered = endpoint.supervisor().subscribe();
    let mut in_flight = HashSet::new();
    let mut in_flight_by_ip = HashMap::new();
    let mut workers = JoinSet::new();

    loop {
        if shutdown.is_cancelled() {
            return;
        }

        spawn_discovery_dial_candidates(
            &endpoint,
            &discovery,
            &limits,
            &mut in_flight,
            &mut in_flight_by_ip,
            &mut workers,
        )
        .await;

        tokio::select! {
            biased;
            // Endpoint shutdown cancels this token. The dialer holds an endpoint
            // clone, so the supervisor registration watch never closes on its
            // own; this is the only reliable teardown signal.
            _ = shutdown.cancelled() => return,
            joined = workers.join_next(), if !workers.is_empty() => {
                match joined {
                    Some(Ok(worker_result)) => {
                        in_flight.remove(&worker_result.node_id);
                        release_discovery_in_flight_ips(
                            &mut in_flight_by_ip,
                            &worker_result.reserved_ips,
                        );
                        apply_discovery_dial_result(
                            &discovery,
                            &worker_result.node_id,
                            worker_result.result,
                            &trace,
                        ).await;
                    }
                    Some(Err(error)) => {
                        debug!(?error, "Zakura discovery dial worker failed");
                        metrics::counter!("zakura.p2p.discovery.dial.worker_failed").increment(1);
                    }
                    None => {}
                }
            }
            changed = registered.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            _ = tokio::time::sleep(ZAKURA_DISCOVERY_DIAL_INTERVAL) => {}
        }
    }
}

async fn spawn_discovery_dial_candidates(
    endpoint: &ZakuraEndpoint,
    discovery: &ZakuraDiscoveryHandle,
    limits: &ZakuraLocalLimits,
    in_flight: &mut HashSet<NodeId>,
    in_flight_by_ip: &mut HashMap<IpAddr, usize>,
    workers: &mut JoinSet<DiscoveryDialWorkerResult>,
) {
    if !endpoint.has_native_admission_capacity() {
        return;
    }

    let in_flight_node_ids: Vec<_> = in_flight.iter().copied().collect();
    for candidate in discovery.dial_candidates(&[], &in_flight_node_ids).await {
        if !endpoint.has_native_admission_capacity() {
            return;
        }
        let Some((node_addr, reserved_ips)) =
            discovery_node_addr_with_reserved_ip_capacity(endpoint, &candidate, in_flight_by_ip)
                .await
        else {
            continue;
        };
        let node_id = candidate.node_id;
        if !in_flight.insert(node_id) {
            continue;
        }
        reserve_discovery_in_flight_ips(in_flight_by_ip, &reserved_ips);

        discovery.mark_dial_attempt(&node_id).await;
        metrics::counter!("zakura.p2p.discovery.dial.started").increment(1);
        workers.spawn(run_discovery_dial_once(
            endpoint.clone(),
            node_addr,
            limits.clone(),
            node_id,
            reserved_ips,
        ));
    }
}

async fn discovery_node_addr_with_reserved_ip_capacity(
    endpoint: &ZakuraEndpoint,
    candidate: &ZakuraDiscoveryDialCandidate,
    in_flight_by_ip: &HashMap<IpAddr, usize>,
) -> Option<(NodeAddr, Vec<IpAddr>)> {
    let mut direct_addrs = Vec::new();
    let mut reserved_ips = Vec::new();
    for addr in &candidate.direct_addrs {
        if can_accept_discovery_dial_ip(endpoint, addr.ip(), in_flight_by_ip).await {
            if !reserved_ips.contains(&addr.ip()) {
                reserved_ips.push(addr.ip());
            }
            direct_addrs.push(*addr);
        }
    }

    (!direct_addrs.is_empty()).then(|| {
        (
            NodeAddr::new(candidate.node_id).with_direct_addresses(direct_addrs),
            reserved_ips,
        )
    })
}

async fn can_accept_discovery_dial_ip(
    endpoint: &ZakuraEndpoint,
    remote_ip: IpAddr,
    in_flight_by_ip: &HashMap<IpAddr, usize>,
) -> bool {
    let in_flight = in_flight_by_ip.get(&remote_ip).copied().unwrap_or_default();
    endpoint
        .supervisor()
        .can_accept_remote_ip_with_in_flight(remote_ip, in_flight)
        .await
}

fn reserve_discovery_in_flight_ips(in_flight_by_ip: &mut HashMap<IpAddr, usize>, ips: &[IpAddr]) {
    for ip in ips {
        *in_flight_by_ip.entry(*ip).or_default() += 1;
    }
}

fn release_discovery_in_flight_ips(in_flight_by_ip: &mut HashMap<IpAddr, usize>, ips: &[IpAddr]) {
    for ip in ips {
        let Some(count) = in_flight_by_ip.get_mut(ip) else {
            continue;
        };
        *count = count.saturating_sub(1);
        if *count == 0 {
            in_flight_by_ip.remove(ip);
        }
    }
}

async fn run_discovery_dial_once(
    endpoint: ZakuraEndpoint,
    node_addr: NodeAddr,
    limits: ZakuraLocalLimits,
    node_id: NodeId,
    reserved_ips: Vec<IpAddr>,
) -> DiscoveryDialWorkerResult {
    let Ok(peer_id) = ZakuraPeerId::new(node_id.as_bytes().to_vec()) else {
        return DiscoveryDialWorkerResult {
            node_id,
            reserved_ips,
            result: DiscoveryDialResult::Failed,
        };
    };
    let mut registered = endpoint.supervisor().subscribe();
    let dial = tokio::spawn({
        let endpoint = endpoint.clone();
        async move { native_bootstrap_dial(&endpoint, node_addr, &limits).await }
    });
    tokio::pin!(dial);

    let result = loop {
        if registered
            .borrow_and_update()
            .iter()
            .any(|id| id == &peer_id)
        {
            break wait_for_discovery_registration_to_settle(&mut registered, &peer_id, &mut dial)
                .await;
        }

        tokio::select! {
            dial_result = &mut dial => {
                break match dial_result {
                    // `native_bootstrap_dial` returns `Ok(())` only after the connection
                    // finishes; discovery success is the peer appearing in the registration watch.
                    Ok(Ok(())) => {
                        if registered.borrow_and_update().iter().any(|id| id == &peer_id) {
                            DiscoveryDialResult::ShortLivedRegistered
                        } else {
                            DiscoveryDialResult::Failed
                        }
                    }
                    Ok(Err(ZakuraHandlerError::ResourceLimit(_))) => {
                        DiscoveryDialResult::LocalResourceLimit
                    }
                    Ok(Err(error)) => {
                        debug!(?error, "Zakura discovery dial failed");
                        DiscoveryDialResult::Failed
                    }
                    Err(error) => {
                        debug!(?error, "Zakura discovery dial task failed");
                        DiscoveryDialResult::Failed
                    }
                };
            }
            changed = registered.changed() => {
                if changed.is_err() {
                    break DiscoveryDialResult::Failed;
                }
            }
        }
    };

    DiscoveryDialWorkerResult {
        node_id,
        reserved_ips,
        result,
    }
}

async fn wait_for_discovery_registration_to_settle(
    registered: &mut tokio::sync::watch::Receiver<Vec<ZakuraPeerId>>,
    peer_id: &ZakuraPeerId,
    dial: &mut std::pin::Pin<
        &mut tokio::task::JoinHandle<Result<(), crate::zakura::ZakuraHandlerError>>,
    >,
) -> DiscoveryDialResult {
    let healthy = tokio::time::sleep(ZAKURA_REDIAL_HEALTHY_CONNECTION);
    tokio::pin!(healthy);

    loop {
        if !registered
            .borrow_and_update()
            .iter()
            .any(|id| id == peer_id)
        {
            return DiscoveryDialResult::ShortLivedRegistered;
        }

        tokio::select! {
            dial_result = &mut *dial => {
                return match dial_result {
                    Ok(Ok(())) | Ok(Err(_)) | Err(_) => DiscoveryDialResult::ShortLivedRegistered,
                };
            }
            changed = registered.changed() => {
                if changed.is_err() {
                    return DiscoveryDialResult::Failed;
                }
            }
            _ = &mut healthy => return DiscoveryDialResult::Registered,
        }
    }
}

async fn apply_discovery_dial_result(
    discovery: &ZakuraDiscoveryHandle,
    node_id: &NodeId,
    result: DiscoveryDialResult,
    trace: &crate::zakura::ZakuraTrace,
) {
    trace.emit_with(DISCOVERY_TABLE, |row| {
        row.insert(
            d_trace::EVENT.to_string(),
            serde_json::Value::String(d_trace::DISCOVERY_DIAL_RESULT.to_string()),
        );
        row.insert(
            d_trace::RESULT.to_string(),
            serde_json::Value::String(result.label().to_string()),
        );
        let peer = ZakuraPeerId::new(node_id.as_bytes().to_vec())
            .map(|peer_id| peer_label(&peer_id))
            .ok();
        row.insert(
            d_trace::PEER.to_string(),
            peer.map_or(serde_json::Value::Null, serde_json::Value::String),
        );
    });

    match result {
        DiscoveryDialResult::Registered => {
            discovery.mark_dial_success(node_id).await;
            metrics::counter!("zakura.p2p.discovery.dial.succeeded").increment(1);
        }
        DiscoveryDialResult::ShortLivedRegistered => {
            discovery.mark_short_lived_exchange(node_id).await;
            metrics::counter!("zakura.p2p.discovery.dial.short_lived_registered").increment(1);
        }
        DiscoveryDialResult::Failed => {
            discovery.mark_dial_failure(node_id).await;
            metrics::counter!("zakura.p2p.discovery.dial.failed").increment(1);
        }
        DiscoveryDialResult::LocalResourceLimit => {
            metrics::counter!("zakura.p2p.discovery.dial.local_resource_limit").increment(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer_id(byte: u8) -> ZakuraPeerId {
        ZakuraPeerId::new(vec![byte; 32]).expect("32-byte test peer id is valid")
    }

    #[tokio::test(start_paused = true)]
    async fn registration_drop_before_healthy_threshold_is_short_lived() {
        let peer_id = peer_id(7);
        let (registered_tx, mut registered_rx) = tokio::sync::watch::channel(vec![peer_id.clone()]);
        let dial = tokio::spawn(async {
            std::future::pending::<Result<(), crate::zakura::ZakuraHandlerError>>().await
        });
        tokio::pin!(dial);

        let settled =
            wait_for_discovery_registration_to_settle(&mut registered_rx, &peer_id, &mut dial);
        tokio::pin!(settled);
        tokio::select! {
            biased;
            result = &mut settled => panic!("settled before registration dropped: {result:?}"),
            _ = tokio::task::yield_now() => {}
        }

        registered_tx.send_replace(Vec::new());
        assert_eq!(settled.await, DiscoveryDialResult::ShortLivedRegistered);
    }
}

//! Bootstrap and candidate dial entry points for native discovery.

use std::{net::SocketAddr, str::FromStr};

use iroh::{NodeAddr, NodeId};

use super::{native_dial_supervised, RedialPolicy};
use crate::zakura::{
    ZakuraEndpoint, ZakuraHandlerError, ZakuraLocalLimits, DEFAULT_ZAKURA_REDIAL_INITIAL_BACKOFF,
    DEFAULT_ZAKURA_REDIAL_MAX_BACKOFF,
};

/// Spawn supervised dials for configured native bootstrap peers.
///
/// Returns one task handle per configured peer so the caller can track them
/// under the endpoint shutdown owner; each maintained dial also observes the
/// endpoint shutdown token directly via [`native_dial_supervised`].
pub(crate) fn spawn_native_bootstrap_dialer(
    endpoint: ZakuraEndpoint,
    bootstrap_peers: Vec<String>,
    limits: ZakuraLocalLimits,
) -> Vec<tokio::task::JoinHandle<()>> {
    if bootstrap_peers.is_empty() {
        return Vec::new();
    }

    // Configured bootstrap peers are maintained: keep re-dialing forever so a
    // node whose only peers are over Zakura (`legacy_p2p = false`) tolerates the
    // seed not being up yet at startup and recovers when a peer later drops. The
    // legacy crawler is absent on such a node, so this loop is the only healing
    // path for its seeds.
    let policy = RedialPolicy::maintain(
        DEFAULT_ZAKURA_REDIAL_INITIAL_BACKOFF,
        DEFAULT_ZAKURA_REDIAL_MAX_BACKOFF,
    );

    let mut tasks = Vec::with_capacity(bootstrap_peers.len());
    for entry in bootstrap_peers {
        let endpoint = endpoint.clone();
        let limits = limits.clone();
        tasks.push(tokio::spawn(async move {
            match parse_bootstrap_peer(&entry) {
                Ok(node_addr) => native_dial_supervised(endpoint, node_addr, limits, policy).await,
                Err(error) => tracing::warn!(?error, ?entry, "invalid Zakura bootstrap peer"),
            }
        }));
    }
    tasks
}

pub(crate) async fn native_bootstrap_dial(
    endpoint: &ZakuraEndpoint,
    node_addr: NodeAddr,
    limits: &ZakuraLocalLimits,
) -> Result<(), ZakuraHandlerError> {
    crate::zakura::handler::serve_native_dial_connection(endpoint, node_addr, limits).await
}

pub(crate) fn parse_bootstrap_peer(entry: &str) -> Result<NodeAddr, ZakuraHandlerError> {
    let Some((node_id, direct_addr)) = entry.split_once('@') else {
        return Err(ZakuraHandlerError::InvalidBootstrapPeer);
    };
    let node_id =
        NodeId::from_str(node_id).map_err(|_| ZakuraHandlerError::InvalidBootstrapPeer)?;
    let direct_addr = direct_addr
        .parse::<SocketAddr>()
        .map_err(|_| ZakuraHandlerError::InvalidBootstrapPeer)?;
    Ok(NodeAddr::new(node_id).with_direct_addresses([direct_addr]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_peer_requires_node_id_and_direct_address() {
        assert!(parse_bootstrap_peer("missing-address").is_err());
        assert!(parse_bootstrap_peer("not-a-node@127.0.0.1:8233").is_err());
    }
}

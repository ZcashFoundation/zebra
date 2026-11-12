//! The node's position on the chain, which telemetry backends attach to the signals they send.
//!
//! The sync progress task records it in every build, whichever backends are compiled in.

use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    OnceLock,
};

use zebra_chain::{
    block::Height,
    parameters::{Network, NetworkUpgrade},
};

/// The network this node runs on.
static NETWORK: OnceLock<Network> = OnceLock::new();

/// The best chain tip height, or [`UNKNOWN_TIP_HEIGHT`] until the progress task records one.
///
/// Backends read this instead of the chain tip watch channel, because Sentry calls `before_send`
/// and `before_send_log` from inside tracing, where instrumented chain tip reads would re-enter the
/// tracing layers.
static TIP_HEIGHT: AtomicU32 = AtomicU32::new(UNKNOWN_TIP_HEIGHT);

/// Above `Height::MAX`, so it is never a real height.
const UNKNOWN_TIP_HEIGHT: u32 = u32::MAX;

/// Whether the syncer was close to the chain tip when the progress task last checked.
static NEAR_TIP: AtomicBool = AtomicBool::new(false);

/// The node's position on the chain when the progress task last checked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ChainPosition {
    /// The best chain tip height.
    pub(crate) tip_height: Height,
    /// The network upgrade at the tip.
    pub(crate) network_upgrade: NetworkUpgrade,
    /// Whether the syncer is close to the chain tip.
    pub(crate) near_tip: bool,
}

/// Records the best chain tip height on `network`, and whether the syncer is close to the tip.
pub(crate) fn record(network: &Network, tip_height: Height, near_tip: bool) {
    NETWORK.get_or_init(|| network.clone());
    NEAR_TIP.store(near_tip, Ordering::Relaxed);
    TIP_HEIGHT.store(tip_height.0, Ordering::Relaxed);
}

/// Returns the node's position on the chain, once the progress task has recorded one.
#[cfg_attr(not(feature = "sentry"), allow(dead_code))]
pub(crate) fn chain_position() -> Option<ChainPosition> {
    let network = NETWORK.get()?;
    let tip_height = TIP_HEIGHT.load(Ordering::Relaxed);
    let tip_height = (tip_height != UNKNOWN_TIP_HEIGHT).then_some(Height(tip_height))?;

    Some(ChainPosition {
        tip_height,
        network_upgrade: NetworkUpgrade::current(network, tip_height),
        near_tip: NEAR_TIP.load(Ordering::Relaxed),
    })
}

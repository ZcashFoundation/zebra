//! Native Zakura discovery service and dial supervision.

mod candidate_dialer;
mod dialer;
mod pipe;
mod protocol;
mod redial;
mod runtime;
mod service;

#[cfg(any(test, feature = "zakura-testkit"))]
pub(crate) use candidate_dialer::run_native_discovery_dialer;
pub(crate) use candidate_dialer::{
    insert_static_bootstrap_candidates, spawn_native_discovery_dialer,
};
pub(crate) use dialer::spawn_native_bootstrap_dialer;
pub use protocol::*;
pub(crate) use redial::{native_dial_supervised, RedialPolicy};
pub(crate) use runtime::{build_discovery_handle, default_advertised_services};
pub use service::{DiscoveryPeerSession, DiscoveryService};

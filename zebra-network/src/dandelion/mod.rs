//! Dandelion++ transaction propagation privacy.
//!
//! Implements the Dandelion++ protocol from "Dandelion++: Lightweight
//! Cryptocurrency Networking with Formal Anonymity Guarantees"
//! (Venkatakrishnan et al., 2018, <https://arxiv.org/abs/1805.11060>).
//!
//! # Overview
//!
//! Each transaction entering the mempool is assigned a [`PropagationState`]:
//!
//! - [`PropagationState::Stem`]: the transaction is forwarded to a single
//!   randomly-selected peer (the "stem peer") for this epoch.  It is NOT
//!   advertised to any other peer.
//!
//! - [`PropagationState::Fluff`]: the transaction is broadcast to all peers
//!   using the normal `AdvertiseTransactionIds` flood.
//!
//! The [`DandelionEpochManager`] selects a new stem peer every
//! [`EPOCH_DURATION`] (default 10 minutes ± random jitter) from the set of
//! currently-connected outbound peers.
//!
//! # Integration points
//!
//! 1. **`zebra-node-services` / `MempoolChange`**: a new
//!    [`MempoolChangeKind::StemAdded`] variant is needed to carry stem-phase
//!    transactions separately from fluff-phase ones.  Until that change lands,
//!    this module uses a side-channel [`tokio::sync::mpsc`] channel.
//!
//! 2. **`zebrad/src/components/mempool/gossip.rs`**: the `gossip_mempool_transaction_id`
//!    function should be split into `gossip_stem` (unicast to stem peer) and
//!    the existing `gossip_fluff` (broadcast to all peers).
//!
//! 3. **`zebra-network/src/peer_set/set.rs`**: `PeerSet` needs a
//!    `broadcast_to_peer(peer_key, request)` method that routes a request to a
//!    specific peer rather than load-balancing across the full set.
//!
//! # Current status
//!
//! This module provides the epoch manager and propagation-state types.
//! The `PeerSet` routing extension and the mempool integration are tracked in
//! TODO comments below and in the accompanying ZIP draft.

pub mod epoch;
pub mod state;

pub use epoch::{DandelionEpochManager, EPOCH_DURATION, EPOCH_JITTER};
pub use state::{PropagationState, PropagationStateMap};

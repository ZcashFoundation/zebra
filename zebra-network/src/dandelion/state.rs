//! Per-transaction Dandelion++ propagation state.

use std::time::Instant;

use zebra_chain::transaction::UnminedTxId;

use crate::meta_addr::PeerSocketAddr;

/// How long a transaction stays in [`PropagationState::Stem`] before being
/// force-promoted to fluff if no fluff observation arrives.
///
/// The Dandelion++ paper recommends 30–60 s.  We use the lower bound so that
/// transactions are not delayed significantly under normal conditions.
pub const STEM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The propagation state of a single transaction under Dandelion++.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropagationState {
    /// Stem phase: the transaction is forwarded only to the current epoch's
    /// stem peer.  It MUST NOT be advertised to any other peer.
    Stem {
        /// The stem peer for this epoch.
        ///
        /// Type is [`PeerSocketAddr`] to match the key type `PeerSet` uses for
        /// its `ready_services` map (`D::Key = PeerSocketAddr`, see
        /// `zebra_network::peer_set::set::PeerSet`).  Using a plain
        /// `std::net::SocketAddr` here would not type-check against `PeerSet`.
        stem_peer: PeerSocketAddr,
        /// When this transaction entered stem phase.  Used to enforce
        /// [`STEM_TIMEOUT`].
        entered_at: Instant,
    },
    /// Fluff phase: the transaction is broadcast to all peers normally.
    Fluff,
}

impl PropagationState {
    /// Returns `true` if this transaction is in stem phase and the stem timeout
    /// has not yet elapsed.
    pub fn is_active_stem(&self) -> bool {
        match self {
            PropagationState::Stem { entered_at, .. } => {
                entered_at.elapsed() < STEM_TIMEOUT
            }
            PropagationState::Fluff => false,
        }
    }

    /// Returns `true` if this transaction is in the [`PropagationState::Stem`]
    /// state, regardless of whether the timeout has elapsed.
    ///
    /// Exposure filters (RPC, P2P `MempoolTransactionIds`, `TransactionsById`)
    /// MUST gate on this rather than [`Self::is_active_stem`]: the gossip task
    /// only flips the state to `Fluff` when it actually broadcasts the
    /// transaction (during the timeout sweep, which runs on an interval).  A
    /// transaction whose 30 s timeout has elapsed but which has not yet been
    /// swept is still withheld from the network, so it MUST stay hidden from
    /// these surfaces until the state is `Fluff`.  Using `is_active_stem` here
    /// would un-hide it for up to one sweep interval before the fluff broadcast,
    /// leaking that this node holds an unbroadcast transaction.
    pub fn is_stem(&self) -> bool {
        matches!(self, PropagationState::Stem { .. })
    }

    /// Returns `true` if this transaction must be promoted to fluff (either
    /// because it is already in fluff phase, or because the stem timeout has
    /// elapsed).
    pub fn should_fluff(&self) -> bool {
        !self.is_active_stem()
    }

    /// Returns the stem peer address, if in stem phase.
    pub fn stem_peer(&self) -> Option<PeerSocketAddr> {
        match self {
            PropagationState::Stem { stem_peer, .. } => Some(*stem_peer),
            PropagationState::Fluff => None,
        }
    }
}

/// Tracks the Dandelion++ propagation state for all pending transactions.
///
/// Transactions are keyed by [`UnminedTxId`].  Entries are removed when the
/// transaction is mined or evicted from the mempool.
#[derive(Debug, Default)]
pub struct PropagationStateMap {
    inner: std::collections::HashMap<UnminedTxId, PropagationState>,
}

impl PropagationStateMap {
    /// Returns a new, empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a transaction in stem phase, using the given peer as the stem
    /// target.
    pub fn insert_stem(&mut self, txid: UnminedTxId, stem_peer: PeerSocketAddr) {
        self.inner.insert(
            txid,
            PropagationState::Stem {
                stem_peer,
                entered_at: Instant::now(),
            },
        );
    }

    /// Inserts a transaction directly in fluff phase (e.g. received from a
    /// peer via normal `inv`/`tx` relay — it is already past stem phase from
    /// this node's perspective).
    pub fn insert_fluff(&mut self, txid: UnminedTxId) {
        self.inner.insert(txid, PropagationState::Fluff);
    }

    /// Promotes a transaction to fluff phase.  No-op if already in fluff.
    pub fn promote_to_fluff(&mut self, txid: &UnminedTxId) {
        if let Some(state) = self.inner.get_mut(txid) {
            *state = PropagationState::Fluff;
        }
    }

    /// Returns the current state for a transaction, or `None` if not tracked.
    pub fn get(&self, txid: &UnminedTxId) -> Option<&PropagationState> {
        self.inner.get(txid)
    }

    /// Removes a transaction from the map (e.g. when mined or evicted).
    pub fn remove(&mut self, txid: &UnminedTxId) {
        self.inner.remove(txid);
    }

    /// Returns all transactions currently in the [`PropagationState::Stem`]
    /// state (regardless of whether the timeout has elapsed).
    ///
    /// Used by RPC and P2P handlers to suppress stem-phase transactions from
    /// responses.  Gates on [`PropagationState::is_stem`] rather than
    /// `is_active_stem` so a timed-out-but-not-yet-swept transaction stays
    /// hidden until the gossip task has actually fluff-broadcast it (see the
    /// doc on `is_stem` for why).
    pub fn stem_txids(&self) -> std::collections::HashSet<UnminedTxId> {
        self.inner
            .iter()
            .filter(|(_, state)| state.is_stem())
            .map(|(txid, _)| *txid)
            .collect()
    }

    /// Returns all stem-phase transactions whose timeout has elapsed.
    ///
    /// Callers SHOULD call [`Self::promote_to_fluff`] for each returned txid
    /// and then flush them as normal fluff broadcasts.
    pub fn expired_stem_txids(&self) -> Vec<UnminedTxId> {
        self.inner
            .iter()
            .filter(|(_, state)| matches!(state, PropagationState::Stem { entered_at, .. } if entered_at.elapsed() >= STEM_TIMEOUT))
            .map(|(txid, _)| *txid)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use zebra_chain::transaction::{self, UnminedTxId};

    fn dummy_addr() -> PeerSocketAddr {
        PeerSocketAddr::from_str("127.0.0.1:8233").unwrap()
    }

    fn dummy_txid() -> UnminedTxId {
        // `transaction::Hash` does not derive `Default`; build one explicitly
        // from a zeroed 32-byte array via its `From<[u8; 32]>` impl.
        UnminedTxId::Legacy(transaction::Hash::from([0u8; 32]))
    }

    #[test]
    fn stem_state_is_active_before_timeout() {
        let txid = dummy_txid();
        let mut map = PropagationStateMap::new();
        map.insert_stem(txid, dummy_addr());

        let state = map.get(&txid).unwrap();
        assert!(state.is_active_stem());
        assert!(!state.should_fluff());
        assert_eq!(state.stem_peer(), Some(dummy_addr()));
    }

    #[test]
    fn fluff_state_should_fluff() {
        let txid = dummy_txid();
        let mut map = PropagationStateMap::new();
        map.insert_fluff(txid);

        let state = map.get(&txid).unwrap();
        assert!(!state.is_active_stem());
        assert!(state.should_fluff());
    }

    #[test]
    fn promote_to_fluff_changes_state() {
        let txid = dummy_txid();
        let mut map = PropagationStateMap::new();
        map.insert_stem(txid, dummy_addr());
        map.promote_to_fluff(&txid);

        let state = map.get(&txid).unwrap();
        assert!(state.should_fluff());
    }

    #[test]
    fn remove_clears_entry() {
        let txid = dummy_txid();
        let mut map = PropagationStateMap::new();
        map.insert_stem(txid, dummy_addr());
        map.remove(&txid);
        assert!(map.get(&txid).is_none());
    }

    /// Verify `stem_txids()` returns only the stem-state set.
    #[test]
    fn stem_txids_excludes_fluff() {
        let stem_txid = dummy_txid();
        let fluff_txid = UnminedTxId::Legacy(
            zebra_chain::transaction::Hash::from([3u8; 32]),
        );

        let mut map = PropagationStateMap::new();
        map.insert_stem(stem_txid, dummy_addr());
        map.insert_fluff(fluff_txid);

        let stem = map.stem_txids();
        assert!(stem.contains(&stem_txid), "stem tx should be in stem set");
        assert!(!stem.contains(&fluff_txid), "fluff tx must not be in stem set");
    }

    /// Verify `is_stem()` stays true even after the stem timeout elapses (unlike
    /// `is_active_stem`), so exposure filters keep hiding a timed-out-but-not-
    /// yet-swept transaction until the gossip task flips it to `Fluff`.
    #[test]
    fn is_stem_is_independent_of_timeout() {
        use std::time::Instant;
        // Construct a Stem state whose entered_at is well past the timeout.
        let expired = PropagationState::Stem {
            stem_peer: dummy_addr(),
            entered_at: Instant::now() - STEM_TIMEOUT - std::time::Duration::from_secs(10),
        };
        assert!(!expired.is_active_stem(), "timed-out stem is not *active*");
        assert!(expired.is_stem(), "but it is still in the Stem state → must stay hidden");

        let fluff = PropagationState::Fluff;
        assert!(!fluff.is_stem(), "fluff is never stem");
    }

    /// Verify the Phase 4 filtering contract: `is_active_stem()` returns true
    /// for a freshly-inserted stem tx and false after fluff promotion.
    ///
    /// This mirrors exactly what `Inbound::call(MempoolTransactionIds)` does:
    /// it calls `ps.get(id).map_or(false, |s| s.is_active_stem())` to decide
    /// which txids to strip from the response.
    #[test]
    fn phase4_filter_strips_active_stem_keeps_fluff_and_unknown() {
        let stem_txid = dummy_txid();
        let fluff_txid = UnminedTxId::Legacy(
            zebra_chain::transaction::Hash::from([1u8; 32]),
        );
        let unknown_txid = UnminedTxId::Legacy(
            zebra_chain::transaction::Hash::from([2u8; 32]),
        );

        let mut map = PropagationStateMap::new();
        map.insert_stem(stem_txid, dummy_addr());
        map.insert_fluff(fluff_txid);
        // unknown_txid is never inserted.

        let all_ids = vec![stem_txid, fluff_txid, unknown_txid];

        // Simulate the Phase 4 filter (same predicate as inbound.rs handler).
        let visible: Vec<_> = all_ids.iter()
            .filter(|id| !map.get(id).map_or(false, |s| s.is_active_stem()))
            .copied()
            .collect();

        assert!(
            !visible.contains(&stem_txid),
            "active stem tx must be stripped from MempoolTransactionIds response"
        );
        assert!(
            visible.contains(&fluff_txid),
            "fluff tx must remain visible"
        );
        assert!(
            visible.contains(&unknown_txid),
            "unknown tx (not in propagation map) must remain visible"
        );
    }

    /// Verify that a stem tx becomes visible again after it is promoted to fluff.
    #[test]
    fn phase4_filter_shows_tx_after_fluff_promotion() {
        let txid = dummy_txid();
        let mut map = PropagationStateMap::new();
        map.insert_stem(txid, dummy_addr());

        // Before promotion: must be filtered.
        assert!(
            map.get(&txid).map_or(false, |s| s.is_active_stem()),
            "newly-stemmed tx should be filtered"
        );

        map.promote_to_fluff(&txid);

        // After promotion: must be visible.
        assert!(
            !map.get(&txid).map_or(false, |s| s.is_active_stem()),
            "promoted-to-fluff tx should no longer be filtered"
        );
    }
}

//! Tachyon shielded pool chain-state types.
//!
//! Tachyon has no note commitment tree. Its pool state is a running *anchor*: a
//! Poseidon hash sequence that absorbs each proof stamp's tachygram-set
//! commitment, ticks once through every stamp-less block, and lifts at epoch
//! boundaries. The anchor takes the role that a note commitment tree root has
//! for the Sapling, Orchard, and Ironwood pools.

use std::{fmt, io};

use serde::{Deserialize, Serialize};

use crate::serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize};

#[cfg(all(zcash_unstable = "nu7", feature = "tx_v7"))]
use crate::block::Block;
#[cfg(all(zcash_unstable = "nu7", feature = "tx_v7"))]
use crate::parameters::{Network, NetworkUpgrade};

/// The running anchor of the Tachyon pool after some block.
///
/// Stored as the canonical 32-byte encoding of the underlying Pallas base field
/// element ([`zcash_tachyon::Anchor`]'s wire encoding), so this type stays
/// available when tachyon support is compiled out.
///
/// The [`Default`] value (all-zero bytes) encodes the field zero element: the
/// fold seed of a pool that has not started yet. It is *not* the anchor of any
/// block; the first pool block's epoch lift turns it into tachyon's genesis
/// anchor. Pre-NU7 blocks use the default value as an ignored placeholder in
/// chain-history leaves.
#[derive(Clone, Copy, Default, Eq, PartialEq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Anchor(pub [u8; 32]);

impl fmt::Debug for Anchor {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("tachyon::Anchor")
            .field(&hex::encode(self.0))
            .finish()
    }
}

impl From<[u8; 32]> for Anchor {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl From<Anchor> for [u8; 32] {
    fn from(anchor: Anchor) -> Self {
        anchor.0
    }
}

impl From<&Anchor> for [u8; 32] {
    fn from(anchor: &Anchor) -> Self {
        anchor.0
    }
}

impl ZcashSerialize for Anchor {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0)
    }
}

impl ZcashDeserialize for Anchor {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(Self(reader.read_32_bytes()?))
    }
}

#[cfg(all(zcash_unstable = "nu7", feature = "tx_v7"))]
impl From<zcash_tachyon::Anchor> for Anchor {
    fn from(anchor: zcash_tachyon::Anchor) -> Self {
        let mut bytes = Vec::with_capacity(32);
        anchor
            .write(&mut bytes)
            .expect("write to Vec is infallible");
        Self(
            bytes
                .try_into()
                .expect("tachyon anchors always encode as 32 bytes"),
        )
    }
}

#[cfg(all(zcash_unstable = "nu7", feature = "tx_v7"))]
impl Anchor {
    /// Convert to the tachyon crate's anchor type.
    ///
    /// # Panics
    ///
    /// Panics if the bytes are not the canonical encoding of a field element.
    /// Anchors are only constructed from canonical encodings (the fold in
    /// [`Self::advance_with_block`], or Zebra's own storage of its results),
    /// so this can only happen on state corruption.
    fn to_tachyon(self) -> zcash_tachyon::Anchor {
        zcash_tachyon::Anchor::read(&self.0[..])
            .expect("stored tachyon anchors are canonical field element encodings")
    }

    /// Compute the Tachyon pool anchor after `block`.
    ///
    /// `self` is the anchor after the parent block, or [`Anchor::default`]
    /// when `block` is the first pool block (NU7 activation). `pool_height` is
    /// the block's 0-based height above the NU7 activation height, which
    /// drives tachyon's epoch schedule.
    ///
    /// This mirrors tachyon's reference pool fold: an epoch-first pool height
    /// lifts the anchor into its epoch, then each proof stamp's tachygram-set
    /// commitment is absorbed in transaction order, and a block with no proof
    /// stamps ticks the anchor once instead (preserving per-height anchor
    /// uniqueness). Adjunct (pointer-stamped) bundles do not contribute: their
    /// tachygrams are carried by the aggregate's proof stamp.
    pub fn advance_with_block(&self, pool_height: u32, block: &Block) -> Anchor {
        use zcash_tachyon::{BlockHeight, TachygramSetPoly, TachyonBundle};

        let pool_height = BlockHeight(pool_height);

        let mut anchor = self.to_tachyon();
        if pool_height.is_epoch_first() {
            anchor = anchor.next_epoch(pool_height.epoch());
        }

        let mut absorbed_stamp = false;
        for transaction in &block.transactions {
            let Some(shielded_data) = transaction.tachyon_shielded_data() else {
                continue;
            };
            if let TachyonBundle::Proven(bundle) = &shielded_data.0 {
                let commit =
                    TachygramSetPoly::from_iter(bundle.stamp.tachygrams.iter().copied()).commit();
                // `next_stamp` panics on the identity commitment, which no
                // root-encoded tachygram set produces: the set polynomial is
                // monic, so its deterministic commitment is never the identity.
                anchor = anchor.next_stamp(&commit);
                absorbed_stamp = true;
            }
        }

        if !absorbed_stamp {
            anchor = anchor.next_empty();
        }

        Anchor::from(anchor)
    }
}

/// The 0-based Tachyon pool height of `height`: its offset above the NU7
/// activation height.
///
/// Returns `None` if NU7 is not active at `height`, or has no activation
/// height on `network`. The pool fold (and its epoch schedule) is indexed by
/// pool height, not chain height.
#[cfg(all(zcash_unstable = "nu7", feature = "tx_v7"))]
pub fn pool_height(network: &Network, height: crate::block::Height) -> Option<u32> {
    let activation_height = NetworkUpgrade::Nu7.activation_height(network)?;
    height.0.checked_sub(activation_height.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_serialization_round_trip() {
        let anchor = Anchor([42; 32]);
        let mut bytes = Vec::new();
        anchor.zcash_serialize(&mut bytes).unwrap();
        assert_eq!(bytes.len(), 32);
        assert_eq!(Anchor::zcash_deserialize(&bytes[..]).unwrap(), anchor);
    }

    #[cfg(all(zcash_unstable = "nu7", feature = "tx_v7"))]
    mod fold {
        use std::sync::Arc;

        use super::super::*;
        use crate::{block::Block, serialization::ZcashDeserialize};

        /// A block with no tachyon bundles (any pre-NU7 vector block works).
        fn stampless_block() -> Block {
            Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_419200_BYTES[..])
                .expect("block test vector is valid")
        }

        /// The default (zero) anchor is the pre-pool fold seed: lifting it into
        /// epoch 0 yields tachyon's genesis anchor.
        #[test]
        fn default_anchor_is_pre_pool_seed() {
            assert_eq!(
                Anchor::default()
                    .to_tachyon()
                    .next_epoch(zcash_tachyon::EpochIndex(0)),
                zcash_tachyon::Anchor::default(),
            );
        }

        #[test]
        fn tachyon_anchor_conversion_round_trips() {
            let anchor = zcash_tachyon::Anchor::default();
            assert_eq!(Anchor::from(anchor).to_tachyon(), anchor);
        }

        /// The per-block fold matches tachyon's reference semantics for
        /// stamp-less blocks: epoch lift on epoch-first pool heights, then a
        /// single empty tick.
        #[test]
        fn stampless_fold_matches_reference() {
            let block = Arc::new(stampless_block());

            // Pool block 0: the genesis epoch lift, then one empty tick.
            let after_genesis = Anchor::default().advance_with_block(0, &block);
            let expected = zcash_tachyon::Anchor::default().next_empty();
            assert_eq!(after_genesis, Anchor::from(expected));

            // A mid-epoch block only ticks.
            let after_next = after_genesis.advance_with_block(1, &block);
            assert_eq!(after_next, Anchor::from(expected.next_empty()));

            // Consecutive anchors are distinct even without stamps.
            assert_ne!(after_genesis, after_next);

            // An epoch-first pool height lifts into its epoch before ticking.
            let after_boundary = after_next.advance_with_block(4096, &block);
            let expected_boundary = expected
                .next_empty()
                .next_epoch(zcash_tachyon::EpochIndex(1))
                .next_empty();
            assert_eq!(after_boundary, Anchor::from(expected_boundary));
        }
    }
}

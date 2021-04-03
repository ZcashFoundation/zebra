//! Sapling spends for `V4` and `V5` `Transaction`s.
//!
//! Zebra uses a generic spend type for `V4` and `V5` transactions.
//! The anchor change is handled using the `AnchorVariant` type trait.

use std::io;

use crate::{
    block::MAX_BLOCK_BYTES,
    primitives::{
        redjubjub::{self, SpendAuth},
        Groth16Proof,
    },
    serialization::{
        ReadZcashExt, SerializationError, TrustedPreallocate, WriteZcashExt, ZcashDeserialize,
        ZcashSerialize,
    },
};

use super::{commitment, note, tree, AnchorVariant, PerSpendAnchor, SharedAnchor};

/// A _Spend Description_, as described in [protocol specification §7.3][ps].
///
/// # Differences between Transaction Versions
///
/// In `Transaction::V4`, each `Spend` has its own anchor. In `Transaction::V5`,
/// there is a single `shared_anchor` for the entire transaction. This
/// structural difference is modeled using the `AnchorVariant` type trait.
/// A type of `()` means "not present in this transaction version".
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#spendencoding
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Spend<AnchorV: AnchorVariant> {
    /// A value commitment to the value of the input note.
    pub cv: commitment::ValueCommitment,
    /// A root of the Sapling note commitment tree at some block height in the past.
    ///
    /// A type of `()` means "not present in this transaction version".
    pub per_spend_anchor: AnchorV::PerSpend,
    /// The nullifier of the input note.
    pub nullifier: note::Nullifier,
    /// The randomized public key for `spend_auth_sig`.
    pub rk: redjubjub::VerificationKeyBytes<SpendAuth>,
    /// The ZK spend proof.
    pub zkproof: Groth16Proof,
    /// A signature authorizing this spend.
    pub spend_auth_sig: redjubjub::Signature<SpendAuth>,
}

impl From<(Spend<SharedAnchor>, tree::Root)> for Spend<PerSpendAnchor> {
    /// Convert a `Spend<SharedAnchor>` and its shared anchor, into a
    /// `Spend<PerSpendAnchor>`.
    fn from(shared_spend: (Spend<SharedAnchor>, tree::Root)) -> Self {
        Spend::<PerSpendAnchor> {
            per_spend_anchor: shared_spend.1,
            cv: shared_spend.0.cv,
            nullifier: shared_spend.0.nullifier,
            rk: shared_spend.0.rk,
            zkproof: shared_spend.0.zkproof,
            spend_auth_sig: shared_spend.0.spend_auth_sig,
        }
    }
}

impl From<(Spend<PerSpendAnchor>, ())> for Spend<PerSpendAnchor> {
    /// Take the `Spend<PerSpendAnchor>` from a spend + anchor tuple.
    fn from(per_spend: (Spend<PerSpendAnchor>, ())) -> Self {
        per_spend.0
    }
}

impl Spend<PerSpendAnchor> {
    /// Encodes the primary inputs for the proof statement as 7 Bls12_381 base
    /// field elements, to match bellman::groth16::verify_proof.
    ///
    /// NB: jubjub::Fq is a type alias for bls12_381::Scalar.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#cctsaplingspend
    pub fn primary_inputs(&self) -> Vec<jubjub::Fq> {
        let mut inputs = vec![];

        let rk_affine = jubjub::AffinePoint::from_bytes(self.rk.into()).unwrap();
        inputs.push(rk_affine.get_u());
        inputs.push(rk_affine.get_v());

        let cv_affine = jubjub::AffinePoint::from_bytes(self.cv.into()).unwrap();
        inputs.push(cv_affine.get_u());
        inputs.push(cv_affine.get_v());

        // TODO: V4 only
        inputs.push(jubjub::Fq::from_bytes(&self.per_spend_anchor.into()).unwrap());

        let nullifier_limbs: [jubjub::Fq; 2] = self.nullifier.into();

        inputs.push(nullifier_limbs[0]);
        inputs.push(nullifier_limbs[1]);

        inputs
    }
}

impl ZcashSerialize for Spend<PerSpendAnchor> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.cv.zcash_serialize(&mut writer)?;
        // TODO: V4 only
        writer.write_all(&self.per_spend_anchor.0[..])?;
        writer.write_32_bytes(&self.nullifier.into())?;
        writer.write_all(&<[u8; 32]>::from(self.rk)[..])?;
        self.zkproof.zcash_serialize(&mut writer)?;
        writer.write_all(&<[u8; 64]>::from(self.spend_auth_sig)[..])?;
        Ok(())
    }
}

impl ZcashDeserialize for Spend<PerSpendAnchor> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        use crate::sapling::{commitment::ValueCommitment, note::Nullifier};
        Ok(Spend {
            cv: ValueCommitment::zcash_deserialize(&mut reader)?,
            // TODO: V4 only
            per_spend_anchor: tree::Root(reader.read_32_bytes()?),
            nullifier: Nullifier::from(reader.read_32_bytes()?),
            rk: reader.read_32_bytes()?.into(),
            zkproof: Groth16Proof::zcash_deserialize(&mut reader)?,
            spend_auth_sig: reader.read_64_bytes()?.into(),
        })
    }
}

/// A Spend contains: a 32 byte cv, a 32 byte anchor, a 32 byte nullifier,  
/// a 32 byte rk, a 192 byte zkproof, and a 64 byte spendAuthSig
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#spendencoding
const SPEND_SIZE: u64 = 32 + 32 + 32 + 32 + 192 + 64;

/// The maximum number of spends in a valid Zcash on-chain transaction.
///
/// If a transaction contains more spends than can fit in maximally large block, it might be
/// valid on the network and in the mempool, but it can never be mined into a block. So
/// rejecting these large edge-case transactions can never break consensus.
impl TrustedPreallocate for Spend {
    fn max_allocation() -> u64 {
        // Since a serialized Vec<Spend> uses at least one byte for its length,
        // the max allocation can never exceed (MAX_BLOCK_BYTES - 1) / SPEND_SIZE
        (MAX_BLOCK_BYTES - 1) / SPEND_SIZE
    }
}

#[cfg(test)]
mod test_trusted_preallocate {
    use super::{Spend, MAX_BLOCK_BYTES, SPEND_SIZE};
    use crate::serialization::{TrustedPreallocate, ZcashSerialize};
    use proptest::prelude::*;
    use std::convert::TryInto;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10_000))]

        /// Confirm that each spend takes exactly SPEND_SIZE bytes when serialized.
        /// This verifies that our calculated `TrustedPreallocate::max_allocation()` is indeed an upper bound.
        #[test]
        fn spend_size_is_small_enough(spend in Spend::arbitrary_with(())) {
            let serialized = spend.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
            prop_assert!(serialized.len() as u64 == SPEND_SIZE)
        }

    }
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        /// Verify that...
        /// 1. The smallest disallowed vector of `Spend`s is too large to fit in a Zcash block
        /// 2. The largest allowed vector is small enough to fit in a legal Zcash block
        #[test]
        fn spend_max_allocation_is_big_enough(output in Spend::arbitrary_with(())) {

            let max_allocation: usize = Spend::max_allocation().try_into().unwrap();
            let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
            for _ in 0..(Spend::max_allocation()+1) {
                smallest_disallowed_vec.push(output.clone());
            }
            let smallest_disallowed_serialized = smallest_disallowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
            // Check that our smallest_disallowed_vec is only one item larger than the limit
            prop_assert!(((smallest_disallowed_vec.len() - 1) as u64) == Spend::max_allocation());
            // Check that our smallest_disallowed_vec is too big to send as a protocol message
            // Note that a serialized block always includes at least one byte for the number of transactions,
            // so any serialized Vec<Spend> at least MAX_BLOCK_BYTES long is too large to fit in a block.
            prop_assert!((smallest_disallowed_serialized.len() as u64) >= MAX_BLOCK_BYTES);

            // Create largest_allowed_vec by removing one element from smallest_disallowed_vec without copying (for efficiency)
            smallest_disallowed_vec.pop();
            let largest_allowed_vec = smallest_disallowed_vec;
            let largest_allowed_serialized = largest_allowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");

            // Check that our largest_allowed_vec contains the maximum number of spends
            prop_assert!((largest_allowed_vec.len() as u64) == Spend::max_allocation());
            // Check that our largest_allowed_vec is small enough to send as a protocol message
            prop_assert!((largest_allowed_serialized.len() as u64) <= MAX_BLOCK_BYTES);
        }
    }
}

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

use super::{commitment, note, tree, AnchorVariant, FieldNotPresent, PerSpendAnchor, SharedAnchor};

/// A _Spend Description_, as described in [protocol specification §7.3][ps].
///
/// # Differences between Transaction Versions
///
/// In `Transaction::V4`, each `Spend` has its own anchor. In `Transaction::V5`,
/// there is a single `shared_anchor` for the entire transaction. This
/// structural difference is modeled using the `AnchorVariant` type trait.
///
/// `V4` transactions serialize the fields of spends and outputs together.
/// `V5` transactions split them into multiple arrays.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#spendencoding
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Spend<AnchorV: AnchorVariant> {
    /// A value commitment to the value of the input note.
    pub cv: commitment::ValueCommitment,
    /// An anchor for this spend.
    ///
    /// The anchor is the root of the Sapling note commitment tree in a previous
    /// block. This root should be in the best chain for a transaction to be
    /// mined, and it must be in the relevant chain for a transaction to be
    /// valid.
    ///
    /// Some transaction versions have a shared anchor, rather than a per-spend
    /// anchor.
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

/// The serialization prefix fields of a `Spend` in Transaction V5.
///
/// In `V5` transactions, spends are split into multiple arrays, so the prefix,
/// proof, and signature must be serialised and deserialized separately.
///
/// Serialized as `SpendDescriptionV5` in [protocol specification §7.3][ps].
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#spendencoding
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpendPrefixInTransactionV5 {
    /// A value commitment to the value of the input note.
    pub cv: commitment::ValueCommitment,
    /// The nullifier of the input note.
    pub nullifier: note::Nullifier,
    /// The randomized public key for `spend_auth_sig`.
    pub rk: redjubjub::VerificationKeyBytes<SpendAuth>,
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

impl From<(Spend<PerSpendAnchor>, FieldNotPresent)> for Spend<PerSpendAnchor> {
    /// Take the `Spend<PerSpendAnchor>` from a spend + anchor tuple.
    fn from(per_spend: (Spend<PerSpendAnchor>, FieldNotPresent)) -> Self {
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

impl Spend<SharedAnchor> {
    /// Combine the prefix and non-prefix fields from V5 transaction
    /// deserialization.
    pub fn from_v5_parts(
        prefix: SpendPrefixInTransactionV5,
        zkproof: Groth16Proof,
        spend_auth_sig: redjubjub::Signature<SpendAuth>,
    ) -> Spend<SharedAnchor> {
        Spend::<SharedAnchor> {
            cv: prefix.cv,
            per_spend_anchor: FieldNotPresent,
            nullifier: prefix.nullifier,
            rk: prefix.rk,
            zkproof,
            spend_auth_sig,
        }
    }

    /// Split out the prefix and non-prefix fields for V5 transaction
    /// serialization.
    pub fn into_v5_parts(
        self,
    ) -> (
        SpendPrefixInTransactionV5,
        Groth16Proof,
        redjubjub::Signature<SpendAuth>,
    ) {
        let prefix = SpendPrefixInTransactionV5 {
            cv: self.cv,
            nullifier: self.nullifier,
            rk: self.rk,
        };

        (prefix, self.zkproof, self.spend_auth_sig)
    }
}

impl ZcashSerialize for Spend<PerSpendAnchor> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.cv.zcash_serialize(&mut writer)?;
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
        Ok(Spend {
            cv: commitment::ValueCommitment::zcash_deserialize(&mut reader)?,
            per_spend_anchor: tree::Root(reader.read_32_bytes()?),
            nullifier: note::Nullifier::from(reader.read_32_bytes()?),
            rk: reader.read_32_bytes()?.into(),
            zkproof: Groth16Proof::zcash_deserialize(&mut reader)?,
            spend_auth_sig: reader.read_64_bytes()?.into(),
        })
    }
}

// zkproof and spend_auth_sig are deserialized separately, so we can only
// deserialize Spend<SharedAnchor> in the context of a V5 transaction.
//
// Instead, implement serialization and deserialization for the
// Spend<SharedAnchor> prefix fields, which are stored in the same array.

impl ZcashSerialize for SpendPrefixInTransactionV5 {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.cv.zcash_serialize(&mut writer)?;
        writer.write_32_bytes(&self.nullifier.into())?;
        writer.write_all(&<[u8; 32]>::from(self.rk)[..])?;
        Ok(())
    }
}

impl ZcashDeserialize for SpendPrefixInTransactionV5 {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(SpendPrefixInTransactionV5 {
            cv: commitment::ValueCommitment::zcash_deserialize(&mut reader)?,
            nullifier: note::Nullifier::from(reader.read_32_bytes()?),
            rk: reader.read_32_bytes()?.into(),
        })
    }
}

/// In Transaction V5, SpendAuth signatures are serialized and deserialized in a
/// separate array.
impl ZcashSerialize for redjubjub::Signature<SpendAuth> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&<[u8; 64]>::from(*self)[..])?;
        Ok(())
    }
}

impl ZcashDeserialize for redjubjub::Signature<SpendAuth> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(reader.read_64_bytes()?.into())
    }
}

/// The size of a spend with a per-spend anchor.
pub(crate) const ANCHOR_PER_SPEND_SIZE: u64 = SHARED_ANCHOR_SPEND_SIZE + 32;

/// The size of a spend with a shared anchor, without associated fields.
///
/// This is the size of spends in the initial array, there are another
/// 2 arrays of zkproofs and spend_auth_sigs required in the transaction format.
pub(crate) const SHARED_ANCHOR_SPEND_PREFIX_SIZE: u64 = 32 + 32 + 32;
/// The size of a spend with a shared anchor, including associated fields.
///
/// A Spend contains: a 32 byte cv, a 32 byte anchor (transaction V4 only),
/// a 32 byte nullifier, a 32 byte rk, a 192 byte zkproof (serialized separately
/// in V5), and a 64 byte spendAuthSig (serialized separately in V5).
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#spendencoding
pub(crate) const SHARED_ANCHOR_SPEND_SIZE: u64 = SHARED_ANCHOR_SPEND_PREFIX_SIZE + 192 + 64;

/// The maximum number of sapling spends in a valid Zcash on-chain transaction V4.
impl TrustedPreallocate for Spend<PerSpendAnchor> {
    fn max_allocation() -> u64 {
        (MAX_BLOCK_BYTES - 1) / ANCHOR_PER_SPEND_SIZE
    }
}

/// The maximum number of sapling spends in a valid Zcash on-chain transaction V5.
///
/// If a transaction contains more spends than can fit in maximally large block, it might be
/// valid on the network and in the mempool, but it can never be mined into a block. So
/// rejecting these large edge-case transactions can never break consensus.
impl TrustedPreallocate for SpendPrefixInTransactionV5 {
    fn max_allocation() -> u64 {
        // Since a serialized Vec<Spend> uses at least one byte for its length,
        // and the associated fields are required,
        // a valid max allocation can never exceed this size
        (MAX_BLOCK_BYTES - 1) / SHARED_ANCHOR_SPEND_SIZE
    }
}

impl TrustedPreallocate for redjubjub::Signature<SpendAuth> {
    fn max_allocation() -> u64 {
        // Each associated field must have a corresponding spend prefix.
        SpendPrefixInTransactionV5::max_allocation()
    }
}

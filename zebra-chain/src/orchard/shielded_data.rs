//! Orchard shielded data for `V5` `Transaction`s.

use std::{
    cmp::{Eq, PartialEq},
    fmt::{self, Debug},
    io,
};

use byteorder::{ReadBytesExt, WriteBytesExt};
use halo2::pasta::pallas;
use reddsa::{orchard::Binding, orchard::SpendAuth, Signature};

use crate::{
    amount::{Amount, NegativeAllowed},
    block::MAX_BLOCK_BYTES,
    orchard::{tree, Action, Nullifier, ValueCommitment},
    primitives::Halo2Proof,
    serialization::{
        AtLeastOne, SerializationError, TrustedPreallocate, ZcashDeserialize, ZcashSerialize,
    },
};

#[cfg(feature = "tx-v6")]
use orchard::{note::AssetBase, value::ValueSum};

use super::{OrchardVanilla, ShieldedDataFlavor};

// FIXME: wrap all ActionGroup usages with tx-v6 feature flag?
/// Action Group description.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(
    not(feature = "tx-v6"),
    serde(bound(serialize = "Flavor::EncryptedNote: serde::Serialize"))
)]
#[cfg_attr(
    feature = "tx-v6",
    serde(bound(
        serialize = "Flavor::EncryptedNote: serde::Serialize, Flavor::BurnType: serde::Serialize",
        deserialize = "Flavor::BurnType: serde::Deserialize<'de>"
    ))
)]
pub struct ActionGroup<Flavor: ShieldedDataFlavor> {
    /// The orchard flags for this transaction.
    /// Denoted as `flagsOrchard` in the spec.
    pub flags: Flags,
    /// The shared anchor for all `Spend`s in this transaction.
    /// Denoted as `anchorOrchard` in the spec.
    pub shared_anchor: tree::Root,
    /// The aggregated zk-SNARK proof for all the actions in this transaction.
    /// Denoted as `proofsOrchard` in the spec.
    pub proof: Halo2Proof,
    /// The Orchard Actions, in the order they appear in the transaction.
    /// Denoted as `vActionsOrchard` and `vSpendAuthSigsOrchard` in the spec.
    pub actions: AtLeastOne<AuthorizedAction<Flavor>>,

    #[cfg(feature = "tx-v6")]
    /// Assets intended for burning
    /// Denoted as `vAssetBurn` in the spec (ZIP 230).
    pub burn: Flavor::BurnType,
}

impl<Flavor: ShieldedDataFlavor> ActionGroup<Flavor> {
    /// Iterate over the [`Action`]s for the [`AuthorizedAction`]s in this
    /// action group, in the order they appear in it.
    pub fn actions(&self) -> impl Iterator<Item = &Action<Flavor>> {
        self.actions.actions()
    }
}

/// A bundle of [`Action`] descriptions and signature data.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(
    not(feature = "tx-v6"),
    serde(bound(serialize = "Flavor::EncryptedNote: serde::Serialize"))
)]
#[cfg_attr(
    feature = "tx-v6",
    serde(bound(
        serialize = "Flavor::EncryptedNote: serde::Serialize, Flavor::BurnType: serde::Serialize",
        deserialize = "Flavor::BurnType: serde::Deserialize<'de>"
    ))
)]
pub struct ShieldedData<Flavor: ShieldedDataFlavor> {
    /// Action Group descriptions.
    /// Denoted as `vActionGroupsOrchard` in the spec (ZIP 230).
    pub action_groups: AtLeastOne<ActionGroup<Flavor>>,
    /// Denoted as `valueBalanceOrchard` in the spec.
    pub value_balance: Amount,
    /// A signature on the transaction `sighash`.
    /// Denoted as `bindingSigOrchard` in the spec.
    pub binding_sig: Signature<Binding>,
}

impl<Flavor: ShieldedDataFlavor> fmt::Display for ActionGroup<Flavor> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut fmter = f.debug_struct("orchard::ActionGroup");

        // FIXME: reorder fields here according the struct/spec?

        fmter.field("actions", &self.actions.len());
        fmter.field("flags", &self.flags);

        fmter.field("proof_len", &self.proof.zcash_serialized_size());

        fmter.field("shared_anchor", &self.shared_anchor);

        #[cfg(feature = "tx-v6")]
        fmter.field("burn", &self.burn.as_ref().len());

        fmter.finish()
    }
}

impl<Flavor: ShieldedDataFlavor> fmt::Display for ShieldedData<Flavor> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut fmter = f.debug_struct("orchard::ShieldedData");

        fmter.field("action_groups", &self.action_groups);
        fmter.field("value_balance", &self.value_balance);

        // FIXME: format binding_sig as well?

        fmter.finish()
    }
}

impl<Flavor: ShieldedDataFlavor> ShieldedData<Flavor> {
    /// Iterate over the [`Action`]s for the [`AuthorizedAction`]s in this
    /// transaction, in the order they appear in it.
    pub fn actions(&self) -> impl Iterator<Item = &Action<Flavor>> {
        self.authorized_actions()
            .map(|authorized_action| &authorized_action.action)
    }

    /// Return an iterator for the [`ActionCommon`] copy of the Actions in this
    /// transaction, in the order they appear in it.
    pub fn action_commons(&self) -> impl Iterator<Item = ActionCommon> + '_ {
        self.actions().map(|action| action.into())
    }

    /// Collect the [`Nullifier`]s for this transaction.
    pub fn nullifiers(&self) -> impl Iterator<Item = &Nullifier> {
        self.actions().map(|action| &action.nullifier)
    }

    /// Calculate the Action binding verification key.
    ///
    /// Getting the binding signature validating key from the Action description
    /// value commitments and the balancing value implicitly checks that the
    /// balancing value is consistent with the value transferred in the
    /// Action descriptions, but also proves that the signer knew the
    /// randomness used for the Action value commitments, which
    /// prevents replays of Action descriptions that perform an output.
    /// In Orchard, all Action descriptions have a spend authorization signature,
    /// therefore the proof of knowledge of the value commitment randomness
    /// is less important, but stills provides defense in depth, and reduces the
    /// differences between Orchard and Sapling.
    ///
    /// The net value of Orchard spends minus outputs in a transaction
    /// is called the balancing value, measured in zatoshi as a signed integer
    /// cv_balance.
    ///
    /// Consistency of cv_balance with the value commitments in Action
    /// descriptions is enforced by the binding signature.
    ///
    /// Instead of generating a key pair at random, we generate it as a function
    /// of the value commitments in the Action descriptions of the transaction, and
    /// the balancing value.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#orchardbalance>
    pub fn binding_verification_key(&self) -> reddsa::VerificationKeyBytes<Binding> {
        let cv: ValueCommitment = self.actions().map(|action| action.cv).sum();

        #[cfg(not(feature = "tx-v6"))]
        let key = {
            let cv_balance = ValueCommitment::new(pallas::Scalar::zero(), self.value_balance);
            cv - cv_balance
        };

        #[cfg(feature = "tx-v6")]
        let key = {
            let cv_balance = ValueCommitment::new(
                pallas::Scalar::zero(),
                // TODO: Make the `ValueSum::from_raw` function public in the `orchard` crate
                // and use `ValueSum::from_raw(self.value_balance.into())` instead of the
                // next line
                (ValueSum::default() + i64::from(self.value_balance)).unwrap(),
                AssetBase::native(),
            );
            let burn_value_commitment = self
                .action_groups
                .iter()
                .map(|action_group| action_group.burn.clone().into())
                .sum::<ValueCommitment>();
            cv - cv_balance - burn_value_commitment
        };

        let key_bytes: [u8; 32] = key.into();

        key_bytes.into()
    }

    /// Provide access to the `value_balance` field of the shielded data.
    ///
    /// Needed to calculate the sapling value balance.
    pub fn value_balance(&self) -> Amount<NegativeAllowed> {
        self.value_balance
    }

    /// Collect the cm_x's for this transaction, if it contains [`Action`]s with
    /// outputs, in the order they appear in the transaction.
    pub fn note_commitments(&self) -> impl Iterator<Item = &pallas::Base> {
        self.actions().map(|action| &action.cm_x)
    }

    /// Makes a union of the flags for this transaction.
    pub fn flags_union(&self) -> Flags {
        self.action_groups
            .iter()
            .map(|action_group| &action_group.flags)
            .fold(Flags::empty(), |result, flags| result.union(*flags))
    }

    /// Collect the shared anchors for this transaction.
    pub fn shared_anchors(&self) -> impl Iterator<Item = tree::Root> + '_ {
        self.action_groups
            .iter()
            .map(|action_group| action_group.shared_anchor)
    }

    /// Iterate over the [`AuthorizedAction`]s in this
    /// transaction, in the order they appear in it.
    pub fn authorized_actions(&self) -> impl Iterator<Item = &AuthorizedAction<Flavor>> {
        self.action_groups
            .iter()
            .flat_map(|action_group| action_group.actions.iter())
    }
}

impl<Flavor: ShieldedDataFlavor> AtLeastOne<AuthorizedAction<Flavor>> {
    /// Iterate over the [`Action`]s of each [`AuthorizedAction`].
    pub fn actions(&self) -> impl Iterator<Item = &Action<Flavor>> {
        self.iter()
            .map(|authorized_action| &authorized_action.action)
    }
}

/// An authorized action description.
///
/// Every authorized Orchard `Action` must have a corresponding `SpendAuth` signature.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(bound = "Flavor::EncryptedNote: serde::Serialize")]
pub struct AuthorizedAction<Flavor: ShieldedDataFlavor> {
    /// The action description of this Action.
    pub action: Action<Flavor>,
    /// The spend signature.
    pub spend_auth_sig: Signature<SpendAuth>,
}

impl<Flavor: ShieldedDataFlavor> AuthorizedAction<Flavor> {
    /// The size of a single Action description.
    ///
    /// Computed as:
    /// ```text
    ///   5 × 32               (fields for nullifier, output commitment, etc.)
    /// + ENC_CIPHERTEXT_SIZE  (580 bytes for OrchardVanilla / Nu5–Nu6,
    ///                        612 bytes for OrchardZSA / Nu7)
    /// + 80                   (authentication tag)
    /// = 820 bytes            (OrchardVanilla)
    /// = 852 bytes            (OrchardZSA)
    /// ```
    ///
    /// - For OrchardVanilla (Nu5/Nu6), ENC_CIPHERTEXT_SIZE = 580; see
    ///   [§ 7.5 Action Description Encoding and Consensus][nu5_pdf] and
    ///   [ZIP-0225 § “Orchard Action Description”][zip225].
    /// - For OrchardZSA (Nu7), ENC_CIPHERTEXT_SIZE = 612; see
    ///   [ZIP-0230 § “OrchardZSA Action Description”][zip230].
    ///
    /// [nu5_pdf]: https://zips.z.cash/protocol/nu5.pdf#actionencodingandconsen
    /// [zip225]: https://zips.z.cash/zip-0225#orchard-action-description-orchardaction
    /// [zip230]: https://zips.z.cash/zip-0230#orchardzsa-action-description-orchardzsaaction
    pub const ACTION_SIZE: u64 = 5 * 32 + (Flavor::ENC_CIPHERTEXT_SIZE as u64) + 80;

    /// The size of a single `Signature<SpendAuth>`.
    ///
    /// Each Signature is 64 bytes.
    /// [7.1 Transaction Encoding and Consensus][ps]
    ///
    /// [ps]: <https://zips.z.cash/protocol/nu5.pdf#actionencodingandconsensus>
    pub const SPEND_AUTH_SIG_SIZE: u64 = 64;

    /// The size of a single AuthorizedAction
    ///
    /// Each serialized `Action` has a corresponding `Signature<SpendAuth>`.
    pub const AUTHORIZED_ACTION_SIZE: u64 = Self::ACTION_SIZE + Self::SPEND_AUTH_SIG_SIZE;

    /// The maximum number of actions allowed in a transaction.
    ///
    /// A serialized `Vec<AuthorizedAction>` requires at least one byte for its length,
    /// and each action must include a signature. Therefore, the maximum allocation
    /// is constrained by these factors and cannot exceed this calculated size.
    pub const ACTION_MAX_ALLOCATION: u64 = (MAX_BLOCK_BYTES - 1) / Self::AUTHORIZED_ACTION_SIZE;

    /// Enforce consensus limit at compile time:
    ///
    /// # Consensus
    ///
    /// > [NU5 onward] nSpendsSapling, nOutputsSapling, and nActionsOrchard MUST all be less than 2^16.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#txnconsensus
    ///
    /// This check works because if `ACTION_MAX_ALLOCATION` were ≥ 2^16, the subtraction below
    /// would underflow for `u64`, causing a compile-time error.
    const _ACTION_MAX_ALLOCATION_OK: u64 = (1 << 16) - Self::ACTION_MAX_ALLOCATION;

    /// Split out the action and the signature for V5 transaction
    /// serialization.
    pub fn into_parts(self) -> (Action<Flavor>, Signature<SpendAuth>) {
        (self.action, self.spend_auth_sig)
    }

    // Combine the action and the spend auth sig from V5 transaction
    /// deserialization.
    pub fn from_parts(
        action: Action<Flavor>,
        spend_auth_sig: Signature<SpendAuth>,
    ) -> AuthorizedAction<Flavor> {
        AuthorizedAction {
            action,
            spend_auth_sig,
        }
    }
}

/// The common field used both in Vanilla actions and ZSA actions.
pub struct ActionCommon {
    /// A value commitment to net value of the input note minus the output note
    pub cv: ValueCommitment,
    /// The nullifier of the input note being spent.
    pub nullifier: super::note::Nullifier,
    /// The randomized validating key for spendAuthSig,
    pub rk: reddsa::VerificationKeyBytes<SpendAuth>,
    /// The x-coordinate of the note commitment for the output note.
    pub cm_x: pallas::Base,
}

impl<Flavor: ShieldedDataFlavor> From<&Action<Flavor>> for ActionCommon {
    fn from(action: &Action<Flavor>) -> Self {
        Self {
            cv: action.cv,
            nullifier: action.nullifier,
            rk: action.rk,
            cm_x: action.cm_x,
        }
    }
}

/// The maximum number of orchard actions in a valid Zcash on-chain transaction V5.
///
/// If a transaction contains more actions than can fit in maximally large block, it might be
/// valid on the network and in the mempool, but it can never be mined into a block. So
/// rejecting these large edge-case transactions can never break consensus.
impl<Flavor: ShieldedDataFlavor> TrustedPreallocate for Action<Flavor> {
    fn max_allocation() -> u64 {
        AuthorizedAction::<Flavor>::ACTION_MAX_ALLOCATION
    }
}

impl TrustedPreallocate for Signature<SpendAuth> {
    fn max_allocation() -> u64 {
        Action::<OrchardVanilla>::max_allocation()
    }
}

bitflags! {
    /// Per-Transaction flags for Orchard.
    ///
    /// The spend and output flags are passed to the `Halo2Proof` verifier, which verifies
    /// the relevant note spending and creation consensus rules.
    ///
    /// # Consensus
    ///
    /// > [NU5 onward] In a version 5 transaction, the reserved bits 2..7 of the flagsOrchard
    /// > field MUST be zero.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
    ///
    /// ([`bitflags`](https://docs.rs/bitflags/1.2.1/bitflags/index.html) restricts its values to the
    /// set of valid flags)
    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    pub struct Flags: u8 {
        /// Enable spending non-zero valued Orchard notes.
        ///
        /// "the `enableSpendsOrchard` flag, if present, MUST be 0 for coinbase transactions"
        const ENABLE_SPENDS = 0b00000001;
        /// Enable creating new non-zero valued Orchard notes.
        const ENABLE_OUTPUTS = 0b00000010;
        /// Enable ZSA transaction (otherwise all notes within actions must use native asset).
        // FIXME: Should we use this flag explicitly anywhere in Zebra?
        const ENABLE_ZSA = 0b00000100;
    }
}

// We use the `bitflags 2.x` library to implement [`Flags`]. The
// `2.x` version of the library uses a different serialization
// format compared to `1.x`.
// This manual implementation uses the `bitflags_serde_legacy` crate
// to serialize `Flags` as `bitflags 1.x` would.
impl serde::Serialize for Flags {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        bitflags_serde_legacy::serialize(self, "Flags", serializer)
    }
}

// We use the `bitflags 2.x` library to implement [`Flags`]. The
// `2.x` version of the library uses a different deserialization
// format compared to `1.x`.
// This manual implementation uses the `bitflags_serde_legacy` crate
// to deserialize `Flags` as `bitflags 1.x` would.
impl<'de> serde::Deserialize<'de> for Flags {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        bitflags_serde_legacy::deserialize("Flags", deserializer)
    }
}

impl ZcashSerialize for Flags {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_u8(self.bits())?;

        Ok(())
    }
}

impl ZcashDeserialize for Flags {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        // Consensus rule: "In a version 5 transaction,
        // the reserved bits 2..7 of the flagsOrchard field MUST be zero."
        // https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus
        Flags::from_bits(reader.read_u8()?)
            .ok_or_else(|| SerializationError::Parse("invalid reserved orchard flags"))
    }
}

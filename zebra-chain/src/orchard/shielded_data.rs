//! Orchard shielded data for `V5` `Transaction`s.

use crate::{
    amount::Amount,
    block::MAX_BLOCK_BYTES,
    orchard::{tree, Action, Nullifier},
    primitives::{
        redpallas::{Binding, Signature, SpendAuth},
        Halo2Proof,
    },
    serialization::{
        AtLeastOne, SerializationError, TrustedPreallocate, ZcashDeserialize, ZcashSerialize,
    },
};

use byteorder::{ReadBytesExt, WriteBytesExt};

use std::{
    cmp::{Eq, PartialEq},
    fmt::Debug,
    io,
};

/// A bundle of [`Action`] descriptions and signature data.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ShieldedData {
    /// The orchard flags for this transaction.
    pub flags: Flags,
    /// The net value of Orchard spends minus outputs.
    pub value_balance: Amount,
    /// The shared anchor for all `Spend`s in this transaction.
    pub shared_anchor: tree::Root,
    /// The aggregated zk-SNARK proof for all the actions in this transaction.
    pub proof: Halo2Proof,
    /// The Orchard Actions.
    pub actions: AtLeastOne<AuthorizedAction>,
    /// A signature on the transaction `sighash`.
    pub binding_sig: Signature<Binding>,
}

impl ShieldedData {
    /// Iterate over the [`Action`]s for the [`AuthorizedAction`]s in this transaction.
    pub fn actions(&self) -> impl Iterator<Item = &Action> {
        self.actions.actions()
    }

    /// Collect the [`Nullifier`]s for this transaction.
    pub fn nullifiers(&self) -> impl Iterator<Item = &Nullifier> {
        self.actions().map(|action| &action.nullifier)
    }
}

impl AtLeastOne<AuthorizedAction> {
    /// Iterate over the [`Action`]s of each [`AuthorizedAction`].
    pub fn actions(&self) -> impl Iterator<Item = &Action> {
        self.iter()
            .map(|authorized_action| &authorized_action.action)
    }
}

/// An authorized action description.
///
/// Every authorized Orchard `Action` must have a corresponding `SpendAuth` signature.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct AuthorizedAction {
    /// The action description of this Action.
    pub action: Action,
    /// The spend signature.
    pub spend_auth_sig: Signature<SpendAuth>,
}

impl AuthorizedAction {
    /// Split out the action and the signature for V5 transaction
    /// serialization.
    pub fn into_parts(self) -> (Action, Signature<SpendAuth>) {
        (self.action, self.spend_auth_sig)
    }

    // Combine the action and the spend auth sig from V5 transaction
    /// deserialization.
    pub fn from_parts(action: Action, spend_auth_sig: Signature<SpendAuth>) -> AuthorizedAction {
        AuthorizedAction {
            action,
            spend_auth_sig,
        }
    }
}

/// The size of a single Action
///
/// Actions are 5 * 32 + 580 + 80 bytes so the total size of each Action is 820 bytes.
/// [7.5 Action Description Encoding and Consensus][ps]
///
/// [ps] https://zips.z.cash/protocol/nu5.pdf#actionencodingandconsensus
pub const ACTION_SIZE: u64 = 5 * 32 + 580 + 80;

/// The size of a single Signature<SpendAuth>
///
/// Each Signature is 64 bytes.
/// [7.1 Transaction Encoding and Consensus][ps]
///
/// [ps] https://zips.z.cash/protocol/nu5.pdf#actionencodingandconsensus
pub const SPEND_AUTH_SIG_SIZE: u64 = 64;

/// The size of a single AuthorizedAction
///
/// Each serialized `Action` has a corresponding `Signature<SpendAuth>`.
pub const AUTHORIZED_ACTION_SIZE: u64 = ACTION_SIZE + SPEND_AUTH_SIG_SIZE;

/// The maximum number of orchard actions in a valid Zcash on-chain transaction V5.
///
/// If a transaction contains more actions than can fit in maximally large block, it might be
/// valid on the network and in the mempool, but it can never be mined into a block. So
/// rejecting these large edge-case transactions can never break consensus.
impl TrustedPreallocate for Action {
    fn max_allocation() -> u64 {
        // Since a serialized Vec<AuthorizedAction> uses at least one byte for its length,
        // and the signature is required,
        // a valid max allocation can never exceed this size
        (MAX_BLOCK_BYTES - 1) / AUTHORIZED_ACTION_SIZE
    }
}

impl TrustedPreallocate for Signature<SpendAuth> {
    fn max_allocation() -> u64 {
        // Each signature must have a corresponding action.
        Action::max_allocation()
    }
}

bitflags! {
    /// Per-Transaction flags for Orchard.
    ///
    /// The spend and output flags are passed to the `Halo2Proof` verifier, which verifies
    /// the relevant note spending and creation consensus rules.
    #[derive(Deserialize, Serialize)]
    pub struct Flags: u8 {
        /// Enable spending non-zero valued Orchard notes.
        ///
        /// "the `enableSpendsOrchard` flag, if present, MUST be 0 for coinbase transactions"
        const ENABLE_SPENDS = 0b00000001;
        /// Enable creating new non-zero valued Orchard notes.
        const ENABLE_OUTPUTS = 0b00000010;
        // Reserved, zeros (bits 2 .. 7)
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
        let flags = Flags::from_bits(reader.read_u8()?).unwrap();

        Ok(flags)
    }
}

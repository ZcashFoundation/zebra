//! Orchard shielded data for `V5` `Transaction`s.

use crate::{
    amount::Amount,
    orchard::{tree, Action},
    primitives::{
        redpallas::{Binding, Signature, SpendAuth},
        Halo2Proof,
    },
    serialization::{AtLeastOne, TrustedPreallocate},
};

use std::{
    cmp::{Eq, PartialEq},
    fmt::Debug,
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

impl TrustedPreallocate for AuthorizedAction {
    fn max_allocation() -> u64 {
        // TODO: fix this
        1_000_000
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

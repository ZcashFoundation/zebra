//! Contains code that interfaces with the zcash_primitives crate from
//! librustzcash.

use std::{
    convert::{TryFrom, TryInto},
    io,
    ops::Deref,
};

use crate::{
    amount::{Amount, NonNegative},
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashSerialize,
    transaction::{AuthDigest, HashType, SigHash, Transaction},
    transparent::{self, Script},
};

// Used by boilerplate code below.

#[derive(Clone, Debug)]
struct TransparentAuth<'a> {
    all_prev_outputs: &'a [transparent::Output],
}

impl zcash_primitives::transaction::components::transparent::Authorization for TransparentAuth<'_> {
    type ScriptSig = zcash_primitives::legacy::Script;
}

// In this block we convert our Output to a librustzcash to TxOut.
// (We could do the serialize/deserialize route but it's simple enough to convert manually)
impl zcash_primitives::transaction::sighash::TransparentAuthorizingContext for TransparentAuth<'_> {
    fn input_amounts(&self) -> Vec<zcash_primitives::transaction::components::amount::Amount> {
        self.all_prev_outputs
            .iter()
            .map(|prevout| {
                zcash_primitives::transaction::components::amount::Amount::from_nonnegative_i64_le_bytes(
                    prevout.value.to_bytes(),
                ).expect("will not fail since it was previously validated")
            })
            .collect()
    }

    fn input_scriptpubkeys(&self) -> Vec<zcash_primitives::legacy::Script> {
        self.all_prev_outputs
            .iter()
            .map(|prevout| {
                zcash_primitives::legacy::Script(prevout.lock_script.as_raw_bytes().into())
            })
            .collect()
    }
}

// Boilerplate mostly copied from `zcash/src/rust/src/transaction_ffi.rs` which is required
// to compute sighash.
// TODO: remove/change if they improve the API to not require this.

struct MapTransparent<'a> {
    auth: TransparentAuth<'a>,
}

impl<'a>
    zcash_primitives::transaction::components::transparent::MapAuth<
        zcash_primitives::transaction::components::transparent::Authorized,
        TransparentAuth<'a>,
    > for MapTransparent<'a>
{
    fn map_script_sig(
        &self,
        s: <zcash_primitives::transaction::components::transparent::Authorized as zcash_primitives::transaction::components::transparent::Authorization>::ScriptSig,
    ) -> <TransparentAuth as zcash_primitives::transaction::components::transparent::Authorization>::ScriptSig{
        s
    }

    fn map_authorization(
        &self,
        _: zcash_primitives::transaction::components::transparent::Authorized,
    ) -> TransparentAuth<'a> {
        // TODO: This map should consume self, so we can move self.auth
        self.auth.clone()
    }
}

struct IdentityMap;

impl
    zcash_primitives::transaction::components::sapling::MapAuth<
        zcash_primitives::transaction::components::sapling::Authorized,
        zcash_primitives::transaction::components::sapling::Authorized,
    > for IdentityMap
{
    fn map_proof(
        &self,
        p: <zcash_primitives::transaction::components::sapling::Authorized as zcash_primitives::transaction::components::sapling::Authorization>::Proof,
    ) -> <zcash_primitives::transaction::components::sapling::Authorized as zcash_primitives::transaction::components::sapling::Authorization>::Proof{
        p
    }

    fn map_auth_sig(
        &self,
        s: <zcash_primitives::transaction::components::sapling::Authorized as zcash_primitives::transaction::components::sapling::Authorization>::AuthSig,
    ) -> <zcash_primitives::transaction::components::sapling::Authorized as zcash_primitives::transaction::components::sapling::Authorization>::AuthSig{
        s
    }

    fn map_authorization(
        &self,
        a: zcash_primitives::transaction::components::sapling::Authorized,
    ) -> zcash_primitives::transaction::components::sapling::Authorized {
        a
    }
}

impl
    zcash_primitives::transaction::components::orchard::MapAuth<
        orchard::bundle::Authorized,
        orchard::bundle::Authorized,
    > for IdentityMap
{
    fn map_spend_auth(
        &self,
        s: <orchard::bundle::Authorized as orchard::bundle::Authorization>::SpendAuth,
    ) -> <orchard::bundle::Authorized as orchard::bundle::Authorization>::SpendAuth {
        s
    }

    fn map_authorization(&self, a: orchard::bundle::Authorized) -> orchard::bundle::Authorized {
        a
    }
}

struct PrecomputedAuth<'a> {
    _phantom: std::marker::PhantomData<&'a ()>,
}

impl<'a> zcash_primitives::transaction::Authorization for PrecomputedAuth<'a> {
    type TransparentAuth = TransparentAuth<'a>;
    type SaplingAuth = zcash_primitives::transaction::components::sapling::Authorized;
    type OrchardAuth = orchard::bundle::Authorized;
}

// End of (mostly) copied code

impl TryFrom<&Transaction> for zcash_primitives::transaction::Transaction {
    type Error = io::Error;

    /// Convert a Zebra transaction into a librustzcash one.
    ///
    /// # Panics
    ///
    /// If the transaction is not V5. (Currently there is no need for this
    /// conversion for other versions.)
    fn try_from(trans: &Transaction) -> Result<Self, Self::Error> {
        let network_upgrade = match trans {
            Transaction::V5 {
                network_upgrade, ..
            } => network_upgrade,
            Transaction::V1 { .. }
            | Transaction::V2 { .. }
            | Transaction::V3 { .. }
            | Transaction::V4 { .. } => panic!("Zebra only uses librustzcash for V5 transactions"),
        };

        convert_tx_to_librustzcash(trans, *network_upgrade)
    }
}

pub(crate) fn convert_tx_to_librustzcash(
    trans: &Transaction,
    network_upgrade: NetworkUpgrade,
) -> Result<zcash_primitives::transaction::Transaction, io::Error> {
    let serialized_tx = trans.zcash_serialize_to_vec()?;
    let branch_id: u32 = network_upgrade
        .branch_id()
        .expect("Network upgrade must have a Branch ID")
        .into();
    // We've already parsed this transaction, so its network upgrade must be valid.
    let branch_id: zcash_primitives::consensus::BranchId = branch_id
        .try_into()
        .expect("zcash_primitives and Zebra have the same branch ids");
    let alt_tx = zcash_primitives::transaction::Transaction::read(&serialized_tx[..], branch_id)?;
    Ok(alt_tx)
}

/// Convert a Zebra Amount into a librustzcash one.
impl TryFrom<Amount<NonNegative>> for zcash_primitives::transaction::components::Amount {
    type Error = ();

    fn try_from(amount: Amount<NonNegative>) -> Result<Self, Self::Error> {
        zcash_primitives::transaction::components::Amount::from_u64(amount.into())
    }
}

/// Convert a Zebra Script into a librustzcash one.
impl From<&Script> for zcash_primitives::legacy::Script {
    fn from(script: &Script) -> Self {
        zcash_primitives::legacy::Script(script.as_raw_bytes().to_vec())
    }
}

/// Compute a signature hash using librustzcash.
///
/// # Inputs
///
/// - `transaction`: the transaction whose signature hash to compute.
/// - `hash_type`: the type of hash (SIGHASH) being used.
/// - `network_upgrade`: the network upgrade of the block containing the transaction.
/// - `input`: information about the transparent input for which this signature
///     hash is being computed, if any. A tuple with the matching output of the
///     previous transaction, the input itself, and the index of the input in
///     the transaction.
pub(crate) fn sighash(
    trans: &Transaction,
    hash_type: HashType,
    network_upgrade: NetworkUpgrade,
    all_previous_outputs: &[transparent::Output],
    input_index: Option<usize>,
) -> SigHash {
    let alt_tx = convert_tx_to_librustzcash(trans, network_upgrade)
        .expect("zcash_primitives and Zebra transaction formats must be compatible");

    let script: zcash_primitives::legacy::Script;
    let signable_input = match input_index {
        Some(input_index) => {
            let output = all_previous_outputs[input_index].clone();
            script = (&output.lock_script).into();
            zcash_primitives::transaction::sighash::SignableInput::Transparent {
                hash_type: hash_type.bits() as _,
                index: input_index,
                script_code: &script,
                script_pubkey: &script,
                value: output
                    .value
                    .try_into()
                    .expect("amount was previously validated"),
            }
        }
        None => zcash_primitives::transaction::sighash::SignableInput::Shielded,
    };

    let txid_parts = alt_tx
        .deref()
        .digest(zcash_primitives::transaction::txid::TxIdDigester);
    let f_transparent = MapTransparent {
        auth: TransparentAuth {
            all_prev_outputs: all_previous_outputs,
        },
    };
    let txdata: zcash_primitives::transaction::TransactionData<PrecomputedAuth> = alt_tx
        .into_data()
        .map_authorization(f_transparent, IdentityMap, IdentityMap);

    SigHash(
        *zcash_primitives::transaction::sighash::signature_hash(
            &txdata,
            &signable_input,
            &txid_parts,
        )
        .as_ref(),
    )
}

/// Compute the authorizing data commitment of this transaction as specified
/// in [ZIP-244].
///
/// # Panics
///
/// If passed a pre-v5 transaction.
///
/// [ZIP-244]: https://zips.z.cash/zip-0244.
pub(crate) fn auth_digest(trans: &Transaction) -> AuthDigest {
    let alt_tx: zcash_primitives::transaction::Transaction = trans
        .try_into()
        .expect("zcash_primitives and Zebra transaction formats must be compatible");

    let digest_bytes: [u8; 32] = alt_tx
        .auth_commitment()
        .as_ref()
        .try_into()
        .expect("digest has the correct size");

    AuthDigest(digest_bytes)
}

/// Return the destination address from a transparent output.
///
/// Returns None if the address type is not valid or unrecognized.
pub(crate) fn transparent_output_address(
    output: &transparent::Output,
    network: Network,
) -> Option<transparent::Address> {
    let script = zcash_primitives::legacy::Script::from(&output.lock_script);
    let alt_addr = script.address();
    match alt_addr {
        Some(zcash_primitives::legacy::TransparentAddress::PublicKey(pub_key_hash)) => Some(
            transparent::Address::from_pub_key_hash(network, pub_key_hash),
        ),
        Some(zcash_primitives::legacy::TransparentAddress::Script(script_hash)) => {
            Some(transparent::Address::from_script_hash(network, script_hash))
        }
        None => None,
    }
}

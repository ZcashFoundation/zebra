//! Contains code that interfaces with the zcash_primitives crate from
//! librustzcash.

use std::{io, ops::Deref};

use zcash_primitives::transaction as zp_tx;

use crate::{
    amount::{Amount, NonNegative},
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashSerialize,
    transaction::{AuthDigest, HashType, SigHash, Transaction},
    transparent::{self, Script},
};

// TODO: move copied and modified code to a separate module.
//
// Used by boilerplate code below.

#[derive(Clone, Debug)]
struct TransparentAuth<'a> {
    all_prev_outputs: &'a [transparent::Output],
}

impl zp_tx::components::transparent::Authorization for TransparentAuth<'_> {
    type ScriptSig = zcash_primitives::legacy::Script;
}

// In this block we convert our Output to a librustzcash to TxOut.
// (We could do the serialize/deserialize route but it's simple enough to convert manually)
impl zp_tx::sighash::TransparentAuthorizingContext for TransparentAuth<'_> {
    fn input_amounts(&self) -> Vec<zp_tx::components::amount::Amount> {
        self.all_prev_outputs
            .iter()
            .map(|prevout| {
                zp_tx::components::amount::Amount::from_nonnegative_i64_le_bytes(
                    prevout.value.to_bytes(),
                )
                .expect("will not fail since it was previously validated")
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
    zp_tx::components::transparent::MapAuth<
        zp_tx::components::transparent::Authorized,
        TransparentAuth<'a>,
    > for MapTransparent<'a>
{
    fn map_script_sig(
        &self,
        s: <zp_tx::components::transparent::Authorized as zp_tx::components::transparent::Authorization>::ScriptSig,
    ) -> <TransparentAuth as zp_tx::components::transparent::Authorization>::ScriptSig {
        s
    }

    fn map_authorization(
        &self,
        _: zp_tx::components::transparent::Authorized,
    ) -> TransparentAuth<'a> {
        // TODO: This map should consume self, so we can move self.auth
        self.auth.clone()
    }
}

struct IdentityMap;

impl
    zp_tx::components::sapling::MapAuth<
        zp_tx::components::sapling::Authorized,
        zp_tx::components::sapling::Authorized,
    > for IdentityMap
{
    fn map_spend_proof(
        &self,
        p: <zp_tx::components::sapling::Authorized as zp_tx::components::sapling::Authorization>::SpendProof,
    ) -> <zp_tx::components::sapling::Authorized as zp_tx::components::sapling::Authorization>::SpendProof
    {
        p
    }

    fn map_output_proof(
        &self,
        p: <zp_tx::components::sapling::Authorized as zp_tx::components::sapling::Authorization>::OutputProof,
    ) -> <zp_tx::components::sapling::Authorized as zp_tx::components::sapling::Authorization>::OutputProof
    {
        p
    }

    fn map_auth_sig(
        &self,
        s: <zp_tx::components::sapling::Authorized as zp_tx::components::sapling::Authorization>::AuthSig,
    ) -> <zp_tx::components::sapling::Authorized as zp_tx::components::sapling::Authorization>::AuthSig{
        s
    }

    fn map_authorization(
        &self,
        a: zp_tx::components::sapling::Authorized,
    ) -> zp_tx::components::sapling::Authorized {
        a
    }
}

impl zp_tx::components::orchard::MapAuth<orchard::bundle::Authorized, orchard::bundle::Authorized>
    for IdentityMap
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

impl<'a> zp_tx::Authorization for PrecomputedAuth<'a> {
    type TransparentAuth = TransparentAuth<'a>;
    type SaplingAuth = zp_tx::components::sapling::Authorized;
    type OrchardAuth = orchard::bundle::Authorized;
}

// End of (mostly) copied code

impl TryFrom<&Transaction> for zp_tx::Transaction {
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
) -> Result<zp_tx::Transaction, io::Error> {
    let serialized_tx = trans.zcash_serialize_to_vec()?;
    let branch_id: u32 = network_upgrade
        .branch_id()
        .expect("Network upgrade must have a Branch ID")
        .into();
    // We've already parsed this transaction, so its network upgrade must be valid.
    let branch_id: zcash_primitives::consensus::BranchId = branch_id
        .try_into()
        .expect("zcash_primitives and Zebra have the same branch ids");
    let alt_tx = zp_tx::Transaction::read(&serialized_tx[..], branch_id)?;
    Ok(alt_tx)
}

/// Convert a Zebra transparent::Output into a librustzcash one.
impl TryFrom<&transparent::Output> for zp_tx::components::TxOut {
    type Error = io::Error;

    #[allow(clippy::unwrap_in_result)]
    fn try_from(output: &transparent::Output) -> Result<Self, Self::Error> {
        let serialized_output_bytes = output
            .zcash_serialize_to_vec()
            .expect("zcash_primitives and Zebra transparent output formats must be compatible");

        zp_tx::components::TxOut::read(&mut serialized_output_bytes.as_slice())
    }
}

/// Convert a Zebra transparent::Output into a librustzcash one.
impl TryFrom<transparent::Output> for zp_tx::components::TxOut {
    type Error = io::Error;

    // The borrow is actually needed to use TryFrom<&transparent::Output>
    #[allow(clippy::needless_borrow)]
    fn try_from(output: transparent::Output) -> Result<Self, Self::Error> {
        (&output).try_into()
    }
}

/// Convert a Zebra Amount into a librustzcash one.
impl TryFrom<Amount<NonNegative>> for zp_tx::components::Amount {
    type Error = ();

    fn try_from(amount: Amount<NonNegative>) -> Result<Self, Self::Error> {
        zp_tx::components::Amount::from_u64(amount.into())
    }
}

/// Convert a Zebra Script into a librustzcash one.
impl From<&Script> for zcash_primitives::legacy::Script {
    fn from(script: &Script) -> Self {
        zcash_primitives::legacy::Script(script.as_raw_bytes().to_vec())
    }
}

/// Convert a Zebra Script into a librustzcash one.
impl From<Script> for zcash_primitives::legacy::Script {
    // The borrow is actually needed to use From<&Script>
    #[allow(clippy::needless_borrow)]
    fn from(script: Script) -> Self {
        (&script).into()
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
            script = output.lock_script.into();
            zp_tx::sighash::SignableInput::Transparent {
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
        None => zp_tx::sighash::SignableInput::Shielded,
    };

    let txid_parts = alt_tx.deref().digest(zp_tx::txid::TxIdDigester);
    let f_transparent = MapTransparent {
        auth: TransparentAuth {
            all_prev_outputs: all_previous_outputs,
        },
    };
    let txdata: zp_tx::TransactionData<PrecomputedAuth> =
        alt_tx
            .into_data()
            .map_authorization(f_transparent, IdentityMap, IdentityMap);

    SigHash(*zp_tx::sighash::signature_hash(&txdata, &signable_input, &txid_parts).as_ref())
}

/// Compute the authorizing data commitment of this transaction as specified
/// in [ZIP-244].
///
/// # Panics
///
/// If passed a pre-v5 transaction.
///
/// [ZIP-244]: https://zips.z.cash/zip-0244
pub(crate) fn auth_digest(trans: &Transaction) -> AuthDigest {
    let alt_tx: zp_tx::Transaction = trans
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
    let tx_out = zp_tx::components::TxOut::try_from(output)
        .expect("zcash_primitives and Zebra transparent output formats must be compatible");

    let alt_addr = tx_out.recipient_address();

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

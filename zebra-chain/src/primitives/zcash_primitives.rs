//! Contains code that interfaces with the zcash_primitives crate from
//! librustzcash.

use std::{io, ops::Deref};

use zcash_primitives::transaction::{self as zp_tx, TxDigests};
use zcash_protocol::value::BalanceError;

use crate::{
    amount::{Amount, NonNegative},
    parameters::{ConsensusBranchId, Network},
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
    fn input_amounts(&self) -> Vec<zp_tx::components::amount::NonNegativeAmount> {
        self.all_prev_outputs
            .iter()
            .map(|prevout| {
                prevout
                    .value
                    .try_into()
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
        sapling_crypto::bundle::Authorized,
        sapling_crypto::bundle::Authorized,
    > for IdentityMap
{
    fn map_spend_proof(
        &mut self,
        p: <sapling_crypto::bundle::Authorized as sapling_crypto::bundle::Authorization>::SpendProof,
    ) -> <sapling_crypto::bundle::Authorized as sapling_crypto::bundle::Authorization>::SpendProof
    {
        p
    }

    fn map_output_proof(
        &mut self,
        p: <sapling_crypto::bundle::Authorized as sapling_crypto::bundle::Authorization>::OutputProof,
    ) -> <sapling_crypto::bundle::Authorized as sapling_crypto::bundle::Authorization>::OutputProof
    {
        p
    }

    fn map_auth_sig(
        &mut self,
        s: <sapling_crypto::bundle::Authorized as sapling_crypto::bundle::Authorization>::AuthSig,
    ) -> <sapling_crypto::bundle::Authorized as sapling_crypto::bundle::Authorization>::AuthSig
    {
        s
    }

    fn map_authorization(
        &mut self,
        a: sapling_crypto::bundle::Authorized,
    ) -> sapling_crypto::bundle::Authorized {
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

#[cfg(zcash_unstable = "nu6" /* TODO nu7 */ )]
impl zp_tx::components::issuance::MapIssueAuth<orchard::issuance::Signed, orchard::issuance::Signed>
    for IdentityMap
{
    fn map_issue_authorization(&self, s: orchard::issuance::Signed) -> orchard::issuance::Signed {
        s
    }
}

#[derive(Debug)]
struct PrecomputedAuth<'a> {
    _phantom: std::marker::PhantomData<&'a ()>,
}

impl<'a> zp_tx::Authorization for PrecomputedAuth<'a> {
    type TransparentAuth = TransparentAuth<'a>;
    type SaplingAuth = sapling_crypto::bundle::Authorized;
    type OrchardAuth = orchard::bundle::Authorized;

    #[cfg(zcash_unstable = "nu6" /* TODO nu7 */ )]
    type IssueAuth = orchard::issuance::Signed;
}

// End of (mostly) copied code

impl TryFrom<&Transaction> for zp_tx::Transaction {
    type Error = io::Error;

    /// Convert a Zebra transaction into a librustzcash one.
    ///
    /// # Panics
    ///
    /// If the transaction is not V5/V6. (Currently there is no need for this
    /// conversion for other versions.)
    #[allow(clippy::unwrap_in_result)]
    fn try_from(trans: &Transaction) -> Result<Self, Self::Error> {
        let network_upgrade = match trans {
            Transaction::V5 {
                network_upgrade, ..
            } => network_upgrade,
            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
                network_upgrade, ..
            } => network_upgrade,
            Transaction::V1 { .. }
            | Transaction::V2 { .. }
            | Transaction::V3 { .. }
            | Transaction::V4 { .. } => panic!("Zebra only uses librustzcash for V5 transactions"),
        };

        convert_tx_to_librustzcash(
            trans,
            network_upgrade.branch_id().expect("V5 txs have branch IDs"),
        )
    }
}

pub(crate) fn convert_tx_to_librustzcash(
    trans: &Transaction,
    branch_id: ConsensusBranchId,
) -> Result<zp_tx::Transaction, io::Error> {
    let serialized_tx = trans.zcash_serialize_to_vec()?;
    let branch_id: u32 = branch_id.into();
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

/// Convert a Zebra non-negative Amount into a librustzcash one.
impl TryFrom<Amount<NonNegative>> for zp_tx::components::amount::NonNegativeAmount {
    type Error = BalanceError;

    fn try_from(amount: Amount<NonNegative>) -> Result<Self, Self::Error> {
        zp_tx::components::amount::NonNegativeAmount::from_nonnegative_i64(amount.into())
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

/// Precomputed data used for sighash or txid computation.
#[derive(Debug)]
pub(crate) struct PrecomputedTxData<'a> {
    tx_data: zp_tx::TransactionData<PrecomputedAuth<'a>>,
    txid_parts: TxDigests<blake2b_simd::Hash>,
    all_previous_outputs: &'a [transparent::Output],
}

impl<'a> PrecomputedTxData<'a> {
    /// Compute data used for sighash or txid computation.
    ///
    /// # Inputs
    ///
    /// - `tx`: the relevant transaction
    /// - `branch_id`: the branch ID of the transaction
    /// - `all_previous_outputs` the transparent Output matching each
    ///   transparent input in the transaction.
    pub(crate) fn new(
        tx: &'a Transaction,
        branch_id: ConsensusBranchId,
        all_previous_outputs: &'a [transparent::Output],
    ) -> PrecomputedTxData<'a> {
        let alt_tx = convert_tx_to_librustzcash(tx, branch_id)
            .expect("zcash_primitives and Zebra transaction formats must be compatible");
        let txid_parts = alt_tx.deref().digest(zp_tx::txid::TxIdDigester);

        let f_transparent = MapTransparent {
            auth: TransparentAuth {
                all_prev_outputs: all_previous_outputs,
            },
        };
        let tx_data: zp_tx::TransactionData<PrecomputedAuth> = alt_tx
            .into_data()
            .map_authorization(f_transparent, IdentityMap, IdentityMap, IdentityMap);

        PrecomputedTxData {
            tx_data,
            txid_parts,
            all_previous_outputs,
        }
    }
}

/// Compute a signature hash using librustzcash.
///
/// # Inputs
///
/// - `precomputed_tx_data`: precomputed data for the transaction whose
///   signature hash is being computed.
/// - `hash_type`: the type of hash (SIGHASH) being used.
/// - `input_index_script_code`: a tuple with the index of the transparent Input
///    for which we are producing a sighash and the respective script code being
///    validated, or None if it's a shielded input.
pub(crate) fn sighash(
    precomputed_tx_data: &PrecomputedTxData,
    hash_type: HashType,
    input_index_script_code: Option<(usize, Vec<u8>)>,
) -> SigHash {
    let lock_script: zcash_primitives::legacy::Script;
    let unlock_script: zcash_primitives::legacy::Script;
    let signable_input = match input_index_script_code {
        Some((input_index, script_code)) => {
            let output = &precomputed_tx_data.all_previous_outputs[input_index];
            lock_script = output.lock_script.clone().into();
            unlock_script = zcash_primitives::legacy::Script(script_code);
            zp_tx::sighash::SignableInput::Transparent {
                hash_type: hash_type.bits() as _,
                index: input_index,
                script_code: &unlock_script,
                script_pubkey: &lock_script,
                value: output
                    .value
                    .try_into()
                    .expect("amount was previously validated"),
            }
        }
        None => zp_tx::sighash::SignableInput::Shielded,
    };

    SigHash(
        *zp_tx::sighash::signature_hash(
            &precomputed_tx_data.tx_data,
            &signable_input,
            &precomputed_tx_data.txid_parts,
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
    network: &Network,
) -> Option<transparent::Address> {
    let tx_out = zp_tx::components::TxOut::try_from(output)
        .expect("zcash_primitives and Zebra transparent output formats must be compatible");

    let alt_addr = tx_out.recipient_address();

    match alt_addr {
        Some(zcash_primitives::legacy::TransparentAddress::PublicKeyHash(pub_key_hash)) => Some(
            transparent::Address::from_pub_key_hash(network.kind(), pub_key_hash),
        ),
        Some(zcash_primitives::legacy::TransparentAddress::ScriptHash(script_hash)) => Some(
            transparent::Address::from_script_hash(network.kind(), script_hash),
        ),
        None => None,
    }
}

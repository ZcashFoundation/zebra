//! Contains code that interfaces with the zcash_primitives crate from
//! librustzcash.

use std::{io, ops::Deref, sync::Arc};

use zcash_primitives::transaction::{self as zp_tx, TxDigests};
use zcash_protocol::value::{BalanceError, ZatBalance, Zatoshis};

use crate::{
    amount::{Amount, NonNegative},
    parameters::NetworkUpgrade,
    serialization::ZcashSerialize,
    transaction::{AuthDigest, HashType, SigHash, Transaction},
    transparent::{self, Script},
    Error,
};

// TODO: move copied and modified code to a separate module.
//
// Used by boilerplate code below.

#[derive(Clone, Debug)]
struct TransparentAuth {
    all_prev_outputs: Arc<Vec<transparent::Output>>,
}

impl zcash_transparent::bundle::Authorization for TransparentAuth {
    type ScriptSig = zcash_primitives::legacy::Script;
}

// In this block we convert our Output to a librustzcash to TxOut.
// (We could do the serialize/deserialize route but it's simple enough to convert manually)
impl zcash_transparent::sighash::TransparentAuthorizingContext for TransparentAuth {
    fn input_amounts(&self) -> Vec<Zatoshis> {
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

struct MapTransparent {
    auth: TransparentAuth,
}

impl zcash_transparent::bundle::MapAuth<zcash_transparent::bundle::Authorized, TransparentAuth>
    for MapTransparent
{
    fn map_script_sig(
        &self,
        s: <zcash_transparent::bundle::Authorized as zcash_transparent::bundle::Authorization>::ScriptSig,
    ) -> <TransparentAuth as zcash_transparent::bundle::Authorization>::ScriptSig {
        s
    }

    fn map_authorization(&self, _: zcash_transparent::bundle::Authorized) -> TransparentAuth {
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

#[derive(Debug)]
struct PrecomputedAuth {}

impl zp_tx::Authorization for PrecomputedAuth {
    type TransparentAuth = TransparentAuth;
    type SaplingAuth = sapling_crypto::bundle::Authorized;
    type OrchardAuth = orchard::bundle::Authorized;
}

// End of (mostly) copied code

/// Convert a Zebra transparent::Output into a librustzcash one.
impl TryFrom<&transparent::Output> for zcash_transparent::bundle::TxOut {
    type Error = io::Error;

    #[allow(clippy::unwrap_in_result)]
    fn try_from(output: &transparent::Output) -> Result<Self, Self::Error> {
        let serialized_output_bytes = output
            .zcash_serialize_to_vec()
            .expect("zcash_primitives and Zebra transparent output formats must be compatible");

        zcash_transparent::bundle::TxOut::read(&mut serialized_output_bytes.as_slice())
    }
}

/// Convert a Zebra transparent::Output into a librustzcash one.
impl TryFrom<transparent::Output> for zcash_transparent::bundle::TxOut {
    type Error = io::Error;

    // The borrow is actually needed to use TryFrom<&transparent::Output>
    #[allow(clippy::needless_borrow)]
    fn try_from(output: transparent::Output) -> Result<Self, Self::Error> {
        (&output).try_into()
    }
}

/// Convert a Zebra non-negative Amount into a librustzcash one.
impl TryFrom<Amount<NonNegative>> for zcash_protocol::value::Zatoshis {
    type Error = BalanceError;

    fn try_from(amount: Amount<NonNegative>) -> Result<Self, Self::Error> {
        zcash_protocol::value::Zatoshis::from_nonnegative_i64(amount.into())
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
pub(crate) struct PrecomputedTxData {
    tx_data: zp_tx::TransactionData<PrecomputedAuth>,
    txid_parts: TxDigests<blake2b_simd::Hash>,
    all_previous_outputs: Arc<Vec<transparent::Output>>,
}

impl PrecomputedTxData {
    /// Computes the data used for sighash or txid computation.
    ///
    /// # Inputs
    ///
    /// - `tx`: the relevant transaction.
    /// - `nu`: the network upgrade to which the transaction belongs.
    /// - `all_previous_outputs`: the transparent Output matching each transparent input in `tx`.
    ///
    /// # Errors
    ///
    /// - If `tx` can't be converted to its `librustzcash` equivalent.
    /// - If `nu` doesn't contain a consensus branch id convertible to its `librustzcash`
    ///   equivalent.
    ///
    /// # Consensus
    ///
    /// > [NU5 only, pre-NU6] All transactions MUST use the NU5 consensus branch ID `0xF919A198` as
    /// > defined in [ZIP-252].
    ///
    /// > [NU6 only] All transactions MUST use the NU6 consensus branch ID `0xC8E71055` as defined
    /// > in  [ZIP-253].
    ///
    /// # Notes
    ///
    /// The check that ensures compliance with the two consensus rules stated above takes place in
    /// the [`Transaction::to_librustzcash`] method. If the check fails, the tx can't be converted
    /// to its `librustzcash` equivalent, which leads to an error. The check relies on the passed
    /// `nu` parameter, which uniquely represents a consensus branch id and can, therefore, be used
    /// as an equivalent to a consensus branch id. The desired `nu` is set either by the script or
    /// tx verifier in `zebra-consensus`.
    ///
    /// [ZIP-252]: <https://zips.z.cash/zip-0252>
    /// [ZIP-253]: <https://zips.z.cash/zip-0253>
    pub(crate) fn new(
        tx: &Transaction,
        nu: NetworkUpgrade,
        all_previous_outputs: Arc<Vec<transparent::Output>>,
    ) -> Result<PrecomputedTxData, Error> {
        let tx = tx.to_librustzcash(nu)?;

        let txid_parts = tx.deref().digest(zp_tx::txid::TxIdDigester);

        let f_transparent = MapTransparent {
            auth: TransparentAuth {
                all_prev_outputs: all_previous_outputs.clone(),
            },
        };

        let tx_data: zp_tx::TransactionData<PrecomputedAuth> =
            tx.into_data()
                .map_authorization(f_transparent, IdentityMap, IdentityMap);

        Ok(PrecomputedTxData {
            tx_data,
            txid_parts,
            all_previous_outputs,
        })
    }

    /// Returns the Orchard bundle in `tx_data`.
    pub fn orchard_bundle(
        &self,
    ) -> Option<orchard::bundle::Bundle<orchard::bundle::Authorized, ZatBalance>> {
        self.tx_data.orchard_bundle().cloned()
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
///   for which we are producing a sighash and the respective script code being
///   validated, or None if it's a shielded input.
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
            zp_tx::sighash::SignableInput::Transparent(
                zcash_transparent::sighash::SignableInput::from_parts(
                    hash_type.try_into().expect("hash type should be ALL"),
                    input_index,
                    &unlock_script,
                    &lock_script,
                    output
                        .value
                        .try_into()
                        .expect("amount was previously validated"),
                ),
            )
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

/// Compute the authorizing data commitment of this transaction as specified in [ZIP-244].
///
/// # Panics
///
/// If passed a pre-v5 transaction.
///
/// [ZIP-244]: https://zips.z.cash/zip-0244
pub(crate) fn auth_digest(tx: &Transaction) -> AuthDigest {
    let nu = tx.network_upgrade().expect("V5 tx has a network upgrade");

    AuthDigest(
        tx.to_librustzcash(nu)
            .expect("V5 tx is convertible to its `zcash_params` equivalent")
            .auth_commitment()
            .as_ref()
            .try_into()
            .expect("digest has the correct size"),
    )
}

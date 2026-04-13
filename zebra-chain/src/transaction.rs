//! Transactions and transaction-related structures.

use std::fmt;

pub use zcash_primitives::transaction::TxVersion;

use zcash_primitives::transaction::{self as zp_tx};
use zcash_protocol::value::ZatBalance;

mod auth_digest;
pub(crate) mod compat;
mod hash;
mod joinsplit;
mod lock_time;
mod memo;
mod serialize;
mod sighash;
mod unmined;

pub mod builder;

#[cfg(any(test, feature = "proptest-impl"))]
#[allow(clippy::unwrap_in_result)]
pub mod arbitrary;
#[cfg(test)]
mod tests;

pub use crate::sapling::FieldNotPresent;
pub use auth_digest::AuthDigest;
pub use hash::{Hash, WtxId};
pub use joinsplit::JoinSplitData;
pub use lock_time::LockTime;
pub use memo::Memo;
pub use serialize::{
    SerializedTransaction, MIN_TRANSPARENT_TX_SIZE, MIN_TRANSPARENT_TX_V4_SIZE,
    MIN_TRANSPARENT_TX_V5_SIZE,
};
pub use sighash::{HashType, SigHash, SigHasher};
pub use unmined::{
    zip317, UnminedTx, UnminedTxId, VerifiedUnminedTx, MEMPOOL_TRANSACTION_COST_THRESHOLD,
};

use crate::{
    amount::{Amount, NegativeAllowed, NonNegative},
    block,
    parameters::NetworkUpgrade,
    transparent,
    value_balance::ValueBalance,
    Error,
};

/// A Zcash transaction, wrapping `zcash_primitives::transaction::Transaction`.
#[derive(Debug)]
pub struct Transaction(pub(crate) zp_tx::Transaction);

impl std::ops::Deref for Transaction {
    type Target = zp_tx::TransactionData<zp_tx::Authorized>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Transaction {
    /// Access the inner `zcash_primitives::transaction::Transaction`.
    pub(crate) fn inner(&self) -> &zp_tx::Transaction {
        &self.0
    }

    /// Returns the transaction version.
    pub fn tx_version(&self) -> TxVersion {
        self.0.version()
    }

    /// Returns the numeric version of this transaction.
    #[allow(unreachable_patterns)]
    pub fn version(&self) -> u32 {
        match self.0.version() {
            TxVersion::Sprout(v) => v,
            TxVersion::V3 => 3,
            TxVersion::V4 => 4,
            TxVersion::V5 => 5,
            #[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
            TxVersion::V6 => 6,
            _ => panic!("unsupported transaction version"),
        }
    }

    /// Returns `true` if this is an overwinter or later transaction.
    pub fn is_overwintered(&self) -> bool {
        !matches!(self.0.version(), TxVersion::Sprout(_))
    }

    /// Get the network upgrade for this transaction, if any (V5+).
    #[allow(unreachable_patterns)]
    pub fn network_upgrade(&self) -> Option<NetworkUpgrade> {
        match self.tx_version() {
            TxVersion::Sprout(_) | TxVersion::V3 | TxVersion::V4 => None,
            // V5+ transactions embed the consensus branch ID
            _ => compat::branch_id_to_network_upgrade(self.0.consensus_branch_id()),
        }
    }

    /// Compute the sighash for this transaction.
    ///
    /// Returns an error if `network_upgrade` doesn't match the transaction's consensus branch ID.
    pub fn sighash(
        &self,
        network_upgrade: NetworkUpgrade,
        hash_type: sighash::HashType,
        all_previous_outputs: std::sync::Arc<Vec<transparent::Output>>,
        input_index_script_code: Option<(usize, Vec<u8>)>,
    ) -> Result<sighash::SigHash, Error> {
        let hasher = sighash::SigHasher::new(self, network_upgrade, all_previous_outputs)?;
        Ok(hasher.sighash(hash_type, input_index_script_code))
    }

    /// Get this transaction's lock time.
    pub fn lock_time(&self) -> Option<LockTime> {
        let lock_time = compat::u32_to_lock_time(self.0.lock_time());

        if lock_time == LockTime::unlocked() {
            return None;
        }

        let has_sequence_number_enabling_lock_time = self
            .inputs()
            .iter()
            .map(transparent::Input::sequence)
            .any(|seq| seq != u32::MAX);

        if has_sequence_number_enabling_lock_time {
            Some(lock_time)
        } else {
            None
        }
    }

    /// Get the raw lock time value as a `u32`.
    pub fn raw_lock_time(&self) -> u32 {
        self.0.lock_time()
    }

    /// Returns `true` if `lock_time` is a [`LockTime::Time`] and is not disabled by sequence numbers.
    pub fn lock_time_is_time(&self) -> bool {
        matches!(self.lock_time(), Some(LockTime::Time(_)))
    }

    /// Get the expiry height for this transaction, if any (V3+).
    ///
    /// Returns `None` if the transaction is Sprout, or if `nExpiryHeight == 0`
    /// (which means "no expiry" per the Zcash protocol spec).
    pub fn expiry_height(&self) -> Option<block::Height> {
        match self.tx_version() {
            TxVersion::Sprout(_) => None,
            _ => {
                let bh = self.0.expiry_height();
                if bh == zcash_primitives::consensus::BlockHeight::from_u32(0) {
                    None
                } else {
                    compat::block_height_to_height(bh).ok()
                }
            }
        }
    }

    /// Get the version group ID for this transaction, if any.
    pub fn version_group_id(&self) -> Option<u32> {
        match self.tx_version() {
            TxVersion::Sprout(_) => None,
            v => Some(v.version_group_id()),
        }
    }

    /// Get the transparent inputs, converted to Zebra types.
    pub fn inputs(&self) -> Vec<transparent::Input> {
        let bundle = self.0.transparent_bundle();
        match bundle {
            Some(b) => b
                .vin
                .iter()
                .map(|txin| {
                    compat::txin_to_input(txin)
                        .expect("librustzcash TxIn should be convertible to Zebra Input")
                })
                .collect(),
            None => Vec::new(),
        }
    }

    /// Get the transparent outputs, converted to Zebra types.
    pub fn outputs(&self) -> Vec<transparent::Output> {
        let bundle = self.0.transparent_bundle();
        match bundle {
            Some(b) => b.vout.iter().map(compat::txout_to_output).collect(),
            None => Vec::new(),
        }
    }

    /// Returns `true` if this transaction has transparent inputs.
    pub fn has_transparent_inputs(&self) -> bool {
        !self.inputs().is_empty()
    }

    /// Returns `true` if this transaction has transparent outputs.
    pub fn has_transparent_outputs(&self) -> bool {
        !self.outputs().is_empty()
    }

    /// Returns `true` if this transaction has transparent inputs or outputs.
    pub fn has_transparent_inputs_or_outputs(&self) -> bool {
        self.has_transparent_inputs() || self.has_transparent_outputs()
    }

    /// Returns `true` if this is a coinbase transaction.
    pub fn is_coinbase(&self) -> bool {
        self.transparent_bundle().is_some_and(|b| b.is_coinbase())
    }

    /// Returns `true` if this is a valid non-coinbase transaction.
    pub fn is_valid_non_coinbase(&self) -> bool {
        !self.is_coinbase()
    }

    /// Returns the outpoints spent by this transaction's transparent inputs.
    pub fn spent_outpoints(&self) -> impl Iterator<Item = transparent::OutPoint> + '_ {
        self.inputs()
            .into_iter()
            .filter_map(|input| input.outpoint())
    }

    /// Compute the hash (txid) of this transaction.
    pub fn hash(&self) -> Hash {
        let txid_bytes: [u8; 32] = *self.0.txid().as_ref();
        Hash(txid_bytes)
    }

    /// Compute the authorizing data commitment for this transaction.
    ///
    /// Returns `None` for pre-V5 transactions (which don't have auth digests).
    pub fn auth_digest(&self) -> Option<AuthDigest> {
        match self.tx_version() {
            TxVersion::Sprout(_) | TxVersion::V3 | TxVersion::V4 => None,
            _ => {
                let hash = self.0.auth_commitment();
                let bytes: &[u8] = hash.as_ref();
                let digest_bytes: [u8; 32] = bytes.try_into().ok()?;
                Some(AuthDigest(digest_bytes))
            }
        }
    }

    /// Compute the unmined transaction ID for this transaction.
    pub fn unmined_id(&self) -> UnminedTxId {
        match self.auth_digest() {
            Some(auth_digest) => UnminedTxId::Witnessed(WtxId {
                id: self.hash(),
                auth_digest,
            }),
            None => UnminedTxId::Legacy(self.hash()),
        }
    }

    /// Returns the number of JoinSplit descriptions in this transaction.
    pub fn joinsplit_count(&self) -> usize {
        self.sprout_bundle().map_or(0, |b| b.joinsplits.len())
    }

    /// Returns `true` if this transaction has Sprout JoinSplit data.
    pub fn has_sprout_joinsplit_data(&self) -> bool {
        self.0.sprout_bundle().is_some()
    }

    /// Iterate over the Sprout JoinSplit descriptions (librustzcash type).
    pub fn sprout_joinsplit_descriptions(
        &self,
    ) -> impl Iterator<Item = &zcash_primitives::transaction::components::sprout::JsDescription> + '_
    {
        self.sprout_bundle()
            .into_iter()
            .flat_map(|b| b.joinsplits.iter())
    }

    /// Access the Sprout nullifiers in this transaction.
    pub fn sprout_nullifiers(&self) -> impl Iterator<Item = crate::sprout::Nullifier> + '_ {
        self.sprout_bundle()
            .into_iter()
            .flat_map(|b| b.joinsplits.iter())
            .flat_map(|js| js.nullifiers().iter().copied())
            .map(crate::sprout::Nullifier::from)
    }

    /// Access the Sprout note commitments in this transaction.
    pub fn sprout_note_commitments(
        &self,
    ) -> impl Iterator<Item = crate::sprout::commitment::NoteCommitment> + '_ {
        self.sprout_bundle()
            .into_iter()
            .flat_map(|b| b.joinsplits.iter())
            .flat_map(|js| js.commitments().iter().copied())
            .map(crate::sprout::commitment::NoteCommitment::from)
    }

    /// Returns vpub_old values (amounts entering the Sprout pool).
    pub fn output_values_to_sprout(&self) -> Vec<i64> {
        self.sprout_bundle()
            .into_iter()
            .flat_map(|b| b.joinsplits.iter())
            .map(|js| js.vpub_old().into())
            .collect()
    }

    /// Returns vpub_new values (amounts leaving the Sprout pool).
    pub fn input_values_from_sprout(&self) -> Vec<i64> {
        self.sprout_bundle()
            .into_iter()
            .flat_map(|b| b.joinsplits.iter())
            .map(|js| js.vpub_new().into())
            .collect()
    }

    /// Access the JoinSplit public validating key, if any.
    pub fn sprout_joinsplit_pub_key(
        &self,
    ) -> Option<crate::primitives::ed25519::VerificationKeyBytes> {
        self.sprout_bundle()
            .map(|b| crate::primitives::ed25519::VerificationKeyBytes::from(b.joinsplit_pubkey))
    }

    /// Returns `true` if this transaction has Sapling shielded data.
    pub fn has_sapling_shielded_data(&self) -> bool {
        self.0.sapling_bundle().is_some()
    }

    /// Access the Sapling nullifiers in this transaction.
    pub fn sapling_nullifiers(&self) -> impl Iterator<Item = crate::sapling::Nullifier> + '_ {
        self.sapling_bundle()
            .into_iter()
            .flat_map(|b| b.shielded_spends().iter())
            .map(|spend| crate::sapling::Nullifier::from(spend.nullifier().0))
    }

    /// Access the Sapling spend descriptions (librustzcash type).
    ///
    /// The spend description type uses `GrothProofBytes` for proofs and
    /// `redjubjub::Signature<SpendAuth>` for auth sigs.
    pub fn sapling_spends(
        &self,
    ) -> impl Iterator<
        Item = &sapling_crypto::bundle::SpendDescription<sapling_crypto::bundle::Authorized>,
    > + '_ {
        self.sapling_bundle()
            .into_iter()
            .flat_map(|b| b.shielded_spends().iter())
    }

    /// Returns the number of Sapling spends.
    pub fn sapling_spends_count(&self) -> usize {
        self.sapling_bundle()
            .map_or(0, |b| b.shielded_spends().len())
    }

    /// Access the Sapling output descriptions (librustzcash type).
    pub fn sapling_outputs(
        &self,
    ) -> impl Iterator<
        Item = &sapling_crypto::bundle::OutputDescription<sapling_crypto::bundle::GrothProofBytes>,
    > + '_ {
        self.sapling_bundle()
            .into_iter()
            .flat_map(|b| b.shielded_outputs().iter())
    }

    /// Access the Sapling note commitments in this transaction.
    pub fn sapling_note_commitments(
        &self,
    ) -> impl Iterator<Item = sapling_crypto::note::ExtractedNoteCommitment> + '_ {
        self.sapling_outputs().map(|output| *output.cmu())
    }

    /// Iterate over deduplicated Sapling anchors as zebra tree roots.
    pub fn sapling_anchors(&self) -> Vec<crate::sapling::tree::Root> {
        let mut seen = Vec::new();
        for spend in self.sapling_spends() {
            let bytes = spend.anchor().to_bytes();
            let root = crate::sapling::tree::Root::try_from(bytes)
                .expect("sapling anchor from valid transaction should be a valid tree root");
            if !seen.contains(&root) {
                seen.push(root);
            }
        }
        seen
    }

    /// Get the Sapling value balance.
    pub fn sapling_value_balance(&self) -> ValueBalance<NegativeAllowed> {
        let balance = self
            .sapling_bundle()
            .map(|b| *b.value_balance())
            .unwrap_or(ZatBalance::zero());

        let amount: Amount<NegativeAllowed> = Amount::try_from(i64::from(balance))
            .expect("sapling value balance should be a valid Amount");

        ValueBalance::from_sapling_amount(amount)
    }

    /// Returns `true` if this transaction has Orchard shielded data.
    pub fn has_orchard_shielded_data(&self) -> bool {
        self.0.orchard_bundle().is_some()
    }

    /// Access the Orchard nullifiers in this transaction.
    pub fn orchard_nullifiers(&self) -> impl Iterator<Item = crate::orchard::Nullifier> + '_ {
        self.orchard_bundle()
            .into_iter()
            .flat_map(|b| b.actions().iter())
            .map(|action| {
                crate::orchard::Nullifier::try_from(action.nullifier().to_bytes())
                    .expect("orchard nullifier from valid transaction")
            })
    }

    /// Access the Orchard actions (librustzcash type).
    pub fn orchard_actions(
        &self,
    ) -> impl Iterator<
        Item = &::orchard::Action<
            <::orchard::bundle::Authorized as ::orchard::bundle::Authorization>::SpendAuth,
        >,
    > + '_ {
        self.orchard_bundle()
            .into_iter()
            .flat_map(|b| b.actions().iter())
    }

    /// Access Orchard note commitments.
    pub fn orchard_note_commitments(
        &self,
    ) -> impl Iterator<Item = ::orchard::note::ExtractedNoteCommitment> + '_ {
        self.orchard_actions().map(|action| *action.cmx())
    }

    /// Access the Orchard flags, if any.
    pub fn orchard_flags(&self) -> Option<::orchard::bundle::Flags> {
        self.0.orchard_bundle().map(|b| *b.flags())
    }

    /// Access the Orchard anchor as a zebra tree root, if any.
    pub fn orchard_anchor(&self) -> Option<crate::orchard::tree::Root> {
        self.0.orchard_bundle().and_then(|b| {
            let bytes = b.anchor().to_bytes();
            crate::orchard::tree::Root::try_from(bytes).ok()
        })
    }

    /// Get the Orchard value balance.
    pub fn orchard_value_balance(&self) -> ValueBalance<NegativeAllowed> {
        let balance = self
            .orchard_bundle()
            .map(|b| *b.value_balance())
            .unwrap_or(ZatBalance::zero());

        let amount: Amount<NegativeAllowed> = Amount::try_from(i64::from(balance))
            .expect("orchard value balance should be a valid Amount");

        ValueBalance::from_orchard_amount(amount)
    }

    /// Returns `true` if this transaction has shielded inputs.
    pub fn has_shielded_inputs(&self) -> bool {
        self.has_sprout_joinsplit_data()
            || self
                .sapling_bundle()
                .is_some_and(|b| !b.shielded_spends().is_empty())
            || self
                .orchard_bundle()
                .is_some_and(|b| b.flags().spends_enabled() && !b.actions().is_empty())
    }

    /// Returns `true` if this transaction has shielded outputs.
    pub fn has_shielded_outputs(&self) -> bool {
        self.has_sprout_joinsplit_data()
            || self
                .sapling_bundle()
                .is_some_and(|b| !b.shielded_outputs().is_empty())
            || self
                .orchard_bundle()
                .is_some_and(|b| b.flags().outputs_enabled() && !b.actions().is_empty())
    }

    /// Does this transaction have shielded inputs or outputs?
    pub fn has_shielded_data(&self) -> bool {
        self.has_shielded_inputs() || self.has_shielded_outputs()
    }

    /// Returns `true` if this transaction has transparent or shielded inputs.
    pub fn has_transparent_or_shielded_inputs(&self) -> bool {
        self.has_transparent_inputs() || self.has_shielded_inputs()
    }

    /// Returns `true` if this transaction has transparent or shielded outputs.
    pub fn has_transparent_or_shielded_outputs(&self) -> bool {
        self.has_transparent_outputs() || self.has_shielded_outputs()
    }

    /// Returns `true` if the Orchard flags are consistent.
    pub fn has_enough_orchard_flags(&self) -> bool {
        match self.0.orchard_bundle() {
            Some(bundle) => {
                let flags = bundle.flags();
                flags.spends_enabled() || flags.outputs_enabled()
            }
            None => true,
        }
    }

    /// Access the zip233 amount field of this transaction.
    pub fn zip233_amount(&self) -> Amount<NonNegative> {
        #[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
        if self.tx_version() == TxVersion::V6 {
            let zatoshis = self.0.zip233_amount();
            let value: u64 = zatoshis.into();
            return Amount::try_from(value as i64)
                .expect("zip233 amount should be a valid non-negative Amount");
        }
        Amount::zero()
    }

    /// Returns `true` if this transaction has a non-zero ZIP-233 amount.
    pub fn has_zip233_amount(&self) -> bool {
        self.zip233_amount() != Amount::<NonNegative>::zero()
    }

    /// Return the transparent value balance,
    /// the change in the transaction value pool due to transparent inputs and outputs.
    #[allow(clippy::unwrap_in_result)]
    pub fn transparent_value_balance_from_outputs(
        &self,
        outputs: &std::collections::HashMap<transparent::OutPoint, transparent::Output>,
    ) -> Result<ValueBalance<NegativeAllowed>, crate::value_balance::ValueBalanceError> {
        use crate::amount::Error as AmountError;

        let input_value = self
            .inputs()
            .iter()
            .map(|i| i.value_from_outputs(outputs))
            .sum::<Result<Amount<NonNegative>, AmountError>>()
            .map_err(crate::value_balance::ValueBalanceError::Transparent)?
            .constrain()
            .expect("conversion from NonNegative to NegativeAllowed is always valid");

        let output_value = self
            .outputs()
            .iter()
            .map(|o| o.value())
            .sum::<Result<Amount<NonNegative>, AmountError>>()
            .map_err(crate::value_balance::ValueBalanceError::Transparent)?
            .constrain()
            .expect("conversion from NonNegative to NegativeAllowed is always valid");

        (input_value - output_value)
            .map(ValueBalance::from_transparent_amount)
            .map_err(crate::value_balance::ValueBalanceError::Transparent)
    }

    /// Return the sprout value balance.
    pub fn sprout_value_balance(&self) -> ValueBalance<NegativeAllowed> {
        let balance = self
            .sprout_bundle()
            .and_then(|b| b.value_balance())
            .unwrap_or(ZatBalance::zero());

        let amount: Amount<NegativeAllowed> = Amount::try_from(i64::from(balance))
            .expect("sprout value balance should be a valid Amount");

        ValueBalance::from_sprout_amount(amount)
    }

    /// Get the overall value balance for this transaction.
    pub fn value_balance(
        &self,
        utxos: &std::collections::HashMap<transparent::OutPoint, transparent::Utxo>,
    ) -> Result<ValueBalance<NegativeAllowed>, crate::value_balance::ValueBalanceError> {
        let transparent = self.transparent_value_balance_from_outputs(
            &transparent::outputs_from_utxos(utxos.clone()),
        )?;
        let sprout = self.sprout_value_balance();
        let sapling = self.sapling_value_balance();
        let orchard = self.orchard_value_balance();

        transparent + sprout + sapling + orchard
    }

    /// Returns the [`transparent::CoinbaseSpendRestriction`] for this transaction,
    /// assuming it is mined at `spend_height`.
    pub fn coinbase_spend_restriction(
        &self,
        network: &crate::parameters::Network,
        spend_height: block::Height,
    ) -> transparent::CoinbaseSpendRestriction {
        if self.outputs().is_empty() || network.should_allow_unshielded_coinbase_spends() {
            transparent::CoinbaseSpendRestriction::CheckCoinbaseMaturity { spend_height }
        } else {
            transparent::CoinbaseSpendRestriction::DisallowCoinbaseSpend
        }
    }
}

impl PartialEq for Transaction {
    fn eq(&self, other: &Self) -> bool {
        self.0.txid() == other.0.txid()
    }
}

impl Eq for Transaction {}

impl std::fmt::Display for Transaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Transaction(v{}, {}, {} tin, {} tout)",
            self.version(),
            self.unmined_id(),
            self.inputs().len(),
            self.outputs().len(),
        )
    }
}

impl From<&Transaction> for Hash {
    fn from(transaction: &Transaction) -> Self {
        transaction.hash()
    }
}

impl From<std::sync::Arc<Transaction>> for Hash {
    fn from(transaction: std::sync::Arc<Transaction>) -> Self {
        transaction.hash()
    }
}

impl From<&Transaction> for UnminedTxId {
    fn from(transaction: &Transaction) -> Self {
        transaction.unmined_id()
    }
}

impl From<std::sync::Arc<Transaction>> for UnminedTxId {
    fn from(transaction: std::sync::Arc<Transaction>) -> Self {
        transaction.unmined_id()
    }
}

impl TryFrom<&Transaction> for AuthDigest {
    type Error = &'static str;

    /// Computes the authorizing data commitment for a transaction.
    ///
    /// Returns an error if passed a pre-V5 transaction (which has no auth digest).
    fn try_from(transaction: &Transaction) -> Result<Self, Self::Error> {
        transaction
            .auth_digest()
            .ok_or("pre-V5 transactions do not have an auth digest")
    }
}

impl crate::serialization::ZcashSerialize for Transaction {
    fn zcash_serialize<W: std::io::Write>(&self, writer: W) -> Result<(), std::io::Error> {
        self.0.write(writer)
    }
}

impl crate::serialization::ZcashDeserializeWithContext<zcash_protocol::consensus::BranchId>
    for Transaction
{
    fn zcash_deserialize_with_context<R: std::io::Read>(
        reader: R,
        &branch_id: &zcash_protocol::consensus::BranchId,
    ) -> Result<Self, crate::serialization::SerializationError> {
        let inner = zp_tx::Transaction::read(reader, branch_id)?;
        Ok(Transaction(inner))
    }
}

impl crate::serialization::ZcashDeserialize for Transaction {
    /// Deserialize a transaction without network context.
    ///
    /// # Branch ID handling
    ///
    /// - **V5+ transactions**: the consensus branch ID is read from the wire. Correct.
    /// - **V1-V4 transactions**: the branch ID is NOT on the wire, so a default
    ///   (`BranchId::Canopy`) is stored.  This does not affect parsing or txid
    ///   computation, but the stored `consensus_branch_id` field will be wrong for
    ///   transactions mined before Canopy.  Use
    ///   `ZcashDeserializeWithContext<BranchId>` when the correct branch ID is known.
    ///
    /// # Callers
    ///
    /// Prefer `ZcashDeserializeWithContext<BranchId>` when the correct branch ID is
    /// known.  This context-free impl exists for:
    /// - Block deserialization (branch ID is corrected afterward)
    /// - Network message parsing and RPC (transactions are re-validated with the
    ///   correct branch ID before sighash computation)
    fn zcash_deserialize<R: std::io::Read>(
        reader: R,
    ) -> Result<Self, crate::serialization::SerializationError> {
        use crate::serialization::ZcashSerialize as _;

        let branch_id = zcash_protocol::consensus::BranchId::Canopy;

        // Limit to MAX_BLOCK_BYTES: a transaction larger than a block is always invalid.
        let limited = reader.take(crate::block::MAX_BLOCK_BYTES);
        // Wrap reader to record bytes as they are consumed, so we can validate
        // the V4 sapling value balance field post-parse without over-reading.
        let mut recording = RecordingReader::new(limited);
        let inner = zp_tx::Transaction::read(&mut recording, branch_id)?;
        let raw_bytes = recording.into_recorded();

        // Validate coinbase inputs: the height encoding must parse correctly.
        // zcash_primitives accepts raw bytes without validating the height encoding,
        // so we validate it explicitly here to preserve Zebra's parse-time check.
        if let Some(bundle) = inner.transparent_bundle() {
            for txin in &bundle.vin {
                if *txin.prevout() == zcash_transparent::bundle::OutPoint::NULL {
                    let script_bytes = txin.script_sig().0 .0.clone();
                    transparent::serialize::parse_coinbase_height(script_bytes)?;
                }
            }
        }

        // For V4 transactions: validate valueBalanceSapling is 0 when there are no
        // sapling spends or outputs. zcash_primitives reads the field but discards it
        // in this case, so we check by re-serializing and comparing with the original.
        if inner.version() == TxVersion::V4 && inner.sapling_bundle().is_none() {
            let tx = Transaction(inner);
            let mut re_serialized = Vec::new();
            tx.zcash_serialize(&mut re_serialized)?;
            if raw_bytes != re_serialized {
                return Err(crate::serialization::SerializationError::BadTransactionBalance);
            }
            return Ok(tx);
        }

        Ok(Transaction(inner))
    }
}

/// An `io::Read` wrapper that records every byte consumed, for post-parse validation.
struct RecordingReader<R> {
    inner: R,
    recorded: Vec<u8>,
}

impl<R: std::io::Read> RecordingReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            recorded: Vec::new(),
        }
    }

    fn into_recorded(self) -> Vec<u8> {
        self.recorded
    }
}

impl<R: std::io::Read> std::io::Read for RecordingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.recorded.extend_from_slice(&buf[..n]);
        Ok(n)
    }
}

impl Clone for Transaction {
    fn clone(&self) -> Self {
        Transaction(self.0.clone())
    }
}

// Human-readable Serialize for elasticsearch, tests, and snapshots.
// Produces structured output matching the old Transaction enum format
// so that RON snapshot tests remain human-readable.
#[cfg(any(test, feature = "proptest-impl", feature = "elasticsearch"))]
impl serde::Serialize for Transaction {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStructVariant;

        let version = self.version();
        let (variant_name, field_count) = match version {
            1 => ("V1", 3),
            2 => ("V2", 4),
            3 => ("V3", 5),
            4 => ("V4", 6),
            _ => ("V5", 7),
        };

        let mut sv = serializer.serialize_struct_variant(
            "Transaction",
            version.saturating_sub(1),
            variant_name,
            field_count,
        )?;

        // V5+ has network_upgrade as the first field (unwrap since V5 always has one)
        if version >= 5 {
            let nu = self
                .network_upgrade()
                .unwrap_or(crate::parameters::NetworkUpgrade::Nu5);
            sv.serialize_field("network_upgrade", &nu)?;
        }

        sv.serialize_field("lock_time", &compat::u32_to_lock_time(self.0.lock_time()))?;

        // V3+ has expiry_height (use Height(0) when nExpiryHeight == 0, matching old format)
        if version >= 3 {
            let eh =
                compat::block_height_to_height(self.0.expiry_height()).unwrap_or(block::Height(0));
            sv.serialize_field("expiry_height", &eh)?;
        }

        sv.serialize_field("inputs", &self.inputs())?;
        sv.serialize_field("outputs", &self.outputs())?;

        if (2..=4).contains(&version) {
            let has_joinsplit = self.has_sprout_joinsplit_data();
            sv.serialize_field::<Option<()>>(
                "joinsplit_data",
                if has_joinsplit { &Some(()) } else { &None },
            )?;
        }

        if version >= 4 {
            let has_sapling = self.has_sapling_shielded_data();
            sv.serialize_field::<Option<()>>(
                "sapling_shielded_data",
                if has_sapling { &Some(()) } else { &None },
            )?;
        }

        if version >= 5 {
            let has_orchard = self.has_orchard_shielded_data();
            sv.serialize_field::<Option<()>>(
                "orchard_shielded_data",
                if has_orchard { &Some(()) } else { &None },
            )?;
        }

        sv.end()
    }
}

#[cfg(any(test, feature = "proptest-impl"))]
impl Transaction {
    /// Build a V1 transaction from transparent components. Used in tests.
    pub fn test_v1(
        inputs: Vec<transparent::Input>,
        outputs: Vec<transparent::Output>,
        lock_time: LockTime,
    ) -> Self {
        Self::build_transparent(
            zcash_primitives::transaction::TxVersion::Sprout(1),
            zcash_protocol::consensus::BranchId::Sprout,
            compat::lock_time_to_u32(&lock_time),
            zcash_primitives::consensus::BlockHeight::from_u32(0),
            inputs,
            outputs,
        )
    }

    /// Build a V2 transaction from transparent components. Used in tests.
    pub fn test_v2(
        inputs: Vec<transparent::Input>,
        outputs: Vec<transparent::Output>,
        lock_time: LockTime,
    ) -> Self {
        Self::build_transparent(
            zcash_primitives::transaction::TxVersion::Sprout(2),
            zcash_protocol::consensus::BranchId::Sprout,
            compat::lock_time_to_u32(&lock_time),
            zcash_primitives::consensus::BlockHeight::from_u32(0),
            inputs,
            outputs,
        )
    }

    /// Build a V3 (Overwinter) transaction from transparent components. Used in tests.
    pub fn test_v3(
        inputs: Vec<transparent::Input>,
        outputs: Vec<transparent::Output>,
        lock_time: LockTime,
        expiry_height: block::Height,
    ) -> Self {
        Self::build_transparent(
            zcash_primitives::transaction::TxVersion::V3,
            zcash_protocol::consensus::BranchId::Overwinter,
            compat::lock_time_to_u32(&lock_time),
            compat::height_to_block_height(expiry_height),
            inputs,
            outputs,
        )
    }

    /// Build a V4 (Sapling) transaction from transparent components. Used in tests.
    pub fn test_v4(
        inputs: Vec<transparent::Input>,
        outputs: Vec<transparent::Output>,
        lock_time: LockTime,
        expiry_height: block::Height,
    ) -> Self {
        Self::build_transparent(
            zcash_primitives::transaction::TxVersion::V4,
            zcash_protocol::consensus::BranchId::Canopy,
            compat::lock_time_to_u32(&lock_time),
            compat::height_to_block_height(expiry_height),
            inputs,
            outputs,
        )
    }

    /// Build a V5 (NU5) transaction from transparent components. Used in tests.
    pub fn test_v5(
        network_upgrade: crate::parameters::NetworkUpgrade,
        inputs: Vec<transparent::Input>,
        outputs: Vec<transparent::Output>,
        lock_time: LockTime,
        expiry_height: block::Height,
    ) -> Self {
        let branch_id = network_upgrade
            .branch_id()
            .and_then(|cbid| zcash_protocol::consensus::BranchId::try_from(cbid).ok())
            .unwrap_or(zcash_protocol::consensus::BranchId::Nu5);
        Self::build_transparent(
            zcash_primitives::transaction::TxVersion::V5,
            branch_id,
            compat::lock_time_to_u32(&lock_time),
            compat::height_to_block_height(expiry_height),
            inputs,
            outputs,
        )
    }

    fn build_transparent(
        version: zcash_primitives::transaction::TxVersion,
        branch_id: zcash_protocol::consensus::BranchId,
        lock_time: u32,
        expiry_height: zcash_primitives::consensus::BlockHeight,
        inputs: Vec<transparent::Input>,
        outputs: Vec<transparent::Output>,
    ) -> Self {
        let vin: Vec<_> = inputs.iter().map(compat::input_to_txin).collect();
        let vout: Vec<_> = outputs.iter().map(compat::output_to_txout).collect();
        let transparent_bundle = if vin.is_empty() && vout.is_empty() {
            None
        } else {
            Some(zcash_transparent::bundle::Bundle {
                vin,
                vout,
                authorization: zcash_transparent::bundle::Authorized,
            })
        };
        let tx_data = zp_tx::TransactionData::from_parts(
            version,
            branch_id,
            lock_time,
            expiry_height,
            transparent_bundle,
            None,
            None,
            None,
        );
        Transaction(tx_data.freeze().expect("built from valid components"))
    }

    /// Rebuild this transaction with new transparent inputs.
    pub fn with_transparent_inputs(self, inputs: Vec<transparent::Input>) -> Self {
        let vin = inputs
            .iter()
            .map(crate::transaction::compat::input_to_txin)
            .collect();
        let vout = self
            .0
            .transparent_bundle()
            .map(|b| b.vout.clone())
            .unwrap_or_default();
        let transparent_bundle = Some(zcash_transparent::bundle::Bundle {
            vin,
            vout,
            authorization: zcash_transparent::bundle::Authorized,
        });
        self.rebuild_with_transparent(transparent_bundle)
    }

    /// Rebuild this transaction with new transparent outputs.
    pub fn with_transparent_outputs(self, outputs: Vec<transparent::Output>) -> Self {
        let vin = self
            .0
            .transparent_bundle()
            .map(|b| b.vin.clone())
            .unwrap_or_default();
        let vout: Vec<_> = outputs
            .iter()
            .map(crate::transaction::compat::output_to_txout)
            .collect();
        let transparent_bundle = if vin.is_empty() && vout.is_empty() {
            None
        } else {
            Some(zcash_transparent::bundle::Bundle {
                vin,
                vout,
                authorization: zcash_transparent::bundle::Authorized,
            })
        };
        self.rebuild_with_transparent(transparent_bundle)
    }

    fn rebuild_with_transparent(
        self,
        transparent_bundle: Option<
            zcash_transparent::bundle::Bundle<zcash_transparent::bundle::Authorized>,
        >,
    ) -> Self {
        let data = &*self.0;
        let tx_data = zp_tx::TransactionData::from_parts(
            data.version(),
            data.consensus_branch_id(),
            data.lock_time(),
            data.expiry_height(),
            transparent_bundle,
            data.sprout_bundle().cloned(),
            data.sapling_bundle().cloned(),
            data.orchard_bundle().cloned(),
        );
        Transaction(tx_data.freeze().expect("rebuilt from valid transaction"))
    }

    /// Rebuild this transaction with a different expiry height (recomputes txid).
    pub fn set_expiry_height(&mut self, height: block::Height) {
        let data = self.0.clone().into_data();
        let new_data = zp_tx::TransactionData::<zp_tx::Authorized>::from_parts(
            data.version(),
            data.consensus_branch_id(),
            data.lock_time(),
            compat::height_to_block_height(height),
            data.transparent_bundle().cloned(),
            data.sprout_bundle().cloned(),
            data.sapling_bundle().cloned(),
            data.orchard_bundle().cloned(),
        );
        self.0 = new_data.freeze().expect("rebuilt from valid transaction");
    }

    /// Rebuild this transaction with a different network upgrade / branch ID (recomputes txid).
    pub fn set_network_upgrade(&mut self, nu: NetworkUpgrade) {
        let branch_id = nu
            .branch_id()
            .and_then(|cbid| zcash_protocol::consensus::BranchId::try_from(cbid).ok())
            .expect("network upgrade must have a valid branch ID");
        let data = self.0.clone().into_data();
        let new_data = zp_tx::TransactionData::<zp_tx::Authorized>::from_parts(
            data.version(),
            branch_id,
            data.lock_time(),
            data.expiry_height(),
            data.transparent_bundle().cloned(),
            data.sprout_bundle().cloned(),
            data.sapling_bundle().cloned(),
            data.orchard_bundle().cloned(),
        );
        self.0 = new_data.freeze().expect("rebuilt from valid transaction");
    }

    /// Replace all transparent outputs (recomputes txid).
    pub fn set_outputs(&mut self, outputs: Vec<transparent::Output>) {
        *self = self.clone().with_transparent_outputs(outputs);
    }

    /// Rebuild this transaction with a replaced Orchard bundle (recomputes txid).
    ///
    /// Test helper for synthesizing transactions with malformed orchard data
    /// (e.g. duplicated actions) that would otherwise be unreachable through
    /// normal construction paths.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn with_orchard_bundle(
        self,
        bundle: Option<::orchard::Bundle<::orchard::bundle::Authorized, ZatBalance>>,
    ) -> Self {
        let data = &*self.0;
        let tx_data = zp_tx::TransactionData::from_parts(
            data.version(),
            data.consensus_branch_id(),
            data.lock_time(),
            data.expiry_height(),
            data.transparent_bundle().cloned(),
            data.sprout_bundle().cloned(),
            data.sapling_bundle().cloned(),
            bundle,
        );
        Transaction(tx_data.freeze().expect("rebuilt from valid transaction"))
    }

    /// Build a V4 transaction with optional JoinSplit data via byte-level serialization.
    ///
    /// Transparent inputs/outputs and sapling shielded data are empty.
    /// Used by tests that need V4 transactions with sprout data.
    pub fn test_v4_with_joinsplit_data(
        joinsplit_data: Option<&JoinSplitData<crate::primitives::Groth16Proof>>,
    ) -> Self {
        use crate::serialization::{ZcashDeserialize, ZcashSerialize};

        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(&0x8000_0004u32.to_le_bytes()); // V4 overwintered
        bytes.extend_from_slice(&0x892F_2085u32.to_le_bytes()); // Sapling versionGroupId
        bytes.push(0x00); // nTransparentInputs
        bytes.push(0x00); // nTransparentOutputs
        bytes.extend_from_slice(&500_000_000u32.to_le_bytes()); // nLockTime
        bytes.extend_from_slice(&0u32.to_le_bytes()); // nExpiryHeight
        bytes.extend_from_slice(&0i64.to_le_bytes()); // valueBalanceSapling
        bytes.push(0x00); // nSpendsSapling
        bytes.push(0x00); // nOutputsSapling
        if let Some(jsd) = joinsplit_data {
            jsd.zcash_serialize(&mut bytes)
                .expect("joinsplit_data serialization should succeed");
        } else {
            bytes.push(0x00); // nJoinSplits
        }
        Transaction::zcash_deserialize(bytes.as_slice())
            .expect("manually constructed V4 transaction should deserialize")
    }
}

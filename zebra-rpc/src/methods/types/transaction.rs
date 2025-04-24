//! The `TransactionTemplate` type is part of the `getblocktemplate` RPC method output.

use zebra_chain::{
    amount::{self, Amount, NegativeOrZero, NonNegative},
    block::merkle::AUTH_DIGEST_PLACEHOLDER,
    transaction::{self, SerializedTransaction, UnminedTx, VerifiedUnminedTx},
};
use zebra_script::CachedFfiTransaction;

/// Transaction data and fields needed to generate blocks using the `getblocktemplate` RPC.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(bound = "FeeConstraint: amount::Constraint + Clone")]
pub struct TransactionTemplate<FeeConstraint>
where
    FeeConstraint: amount::Constraint + Clone,
{
    /// The hex-encoded serialized data for this transaction.
    #[serde(with = "hex")]
    pub data: SerializedTransaction,

    /// The transaction ID of this transaction.
    #[serde(with = "hex")]
    pub(crate) hash: transaction::Hash,

    /// The authorizing data digest of a v5 transaction, or a placeholder for older versions.
    #[serde(rename = "authdigest")]
    #[serde(with = "hex")]
    pub(crate) auth_digest: transaction::AuthDigest,

    /// The transactions in this block template that this transaction depends upon.
    /// These are 1-based indexes in the `transactions` list.
    ///
    /// Zebra's mempool does not support transaction dependencies, so this list is always empty.
    ///
    /// We use `u16` because 2 MB blocks are limited to around 39,000 transactions.
    pub(crate) depends: Vec<u16>,

    /// The fee for this transaction.
    ///
    /// Non-coinbase transactions must be `NonNegative`.
    /// The Coinbase transaction `fee` is the negative sum of the fees of the transactions in
    /// the block, so their fee must be `NegativeOrZero`.
    pub(crate) fee: Amount<FeeConstraint>,

    /// The number of transparent signature operations in this transaction.
    pub(crate) sigops: u64,

    /// Is this transaction required in the block?
    ///
    /// Coinbase transactions are required, all other transactions are not.
    pub(crate) required: bool,
}

// Convert from a mempool transaction to a non-coinbase transaction template.
impl From<&VerifiedUnminedTx> for TransactionTemplate<NonNegative> {
    fn from(tx: &VerifiedUnminedTx) -> Self {
        assert!(
            !tx.transaction.transaction.is_coinbase(),
            "unexpected coinbase transaction in mempool"
        );

        Self {
            data: tx.transaction.transaction.as_ref().into(),
            hash: tx.transaction.id.mined_id(),
            auth_digest: tx
                .transaction
                .id
                .auth_digest()
                .unwrap_or(AUTH_DIGEST_PLACEHOLDER),

            // Always empty, not supported by Zebra's mempool.
            depends: Vec::new(),

            fee: tx.miner_fee,

            sigops: tx.legacy_sigop_count,

            // Zebra does not require any transactions except the coinbase transaction.
            required: false,
        }
    }
}

impl From<VerifiedUnminedTx> for TransactionTemplate<NonNegative> {
    fn from(tx: VerifiedUnminedTx) -> Self {
        Self::from(&tx)
    }
}

impl TransactionTemplate<NegativeOrZero> {
    /// Convert from a generated coinbase transaction into a coinbase transaction template.
    ///
    /// `miner_fee` is the total miner fees for the block, excluding newly created block rewards.
    //
    // TODO: use a different type for generated coinbase transactions?
    pub fn from_coinbase(tx: &UnminedTx, miner_fee: Amount<NonNegative>) -> Self {
        assert!(
            tx.transaction.is_coinbase(),
            "invalid generated coinbase transaction: \
             must have exactly one input, which must be a coinbase input",
        );

        let miner_fee = (-miner_fee)
            .constrain()
            .expect("negating a NonNegative amount always results in a valid NegativeOrZero");

        let legacy_sigop_count = CachedFfiTransaction::new(tx.transaction.clone(), Vec::new())
            .legacy_sigop_count()
            .expect(
                "invalid generated coinbase transaction: \
                 failure in zcash_script sigop count",
            );

        Self {
            data: tx.transaction.as_ref().into(),
            hash: tx.id.mined_id(),
            auth_digest: tx.id.auth_digest().unwrap_or(AUTH_DIGEST_PLACEHOLDER),

            // Always empty, coinbase transactions never have inputs.
            depends: Vec::new(),

            fee: miner_fee,

            sigops: legacy_sigop_count,

            // Zcash requires a coinbase transaction.
            required: true,
        }
    }
}

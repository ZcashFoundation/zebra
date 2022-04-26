//! Errors that can occur when checking consensus rules.
//!
//! Each error variant corresponds to a consensus rule, so enumerating
//! all possible verification failures enumerates the consensus rules we
//! implement, and ensures that we don't reject blocks or transactions
//! for a non-enumerated reason.

use chrono::{DateTime, Utc};
use thiserror::Error;

use zebra_chain::{amount, block, orchard, sapling, sprout, transparent};

use crate::{block::MAX_BLOCK_SIGOPS, BoxError};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// Workaround for format string identifier rules.
const MAX_EXPIRY_HEIGHT: block::Height = block::Height::MAX_EXPIRY_HEIGHT;

#[derive(Error, Copy, Clone, Debug, PartialEq, Eq)]
pub enum SubsidyError {
    #[error("no coinbase transaction in block")]
    NoCoinbase,

    #[error("funding stream expected output not found")]
    FundingStreamNotFound,

    #[error("miner fees are invalid")]
    InvalidMinerFees,

    #[error("a sum of amounts overflowed")]
    SumOverflow,
}

#[derive(Error, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub enum TransactionError {
    #[error("first transaction must be coinbase")]
    CoinbasePosition,

    #[error("coinbase input found in non-coinbase transaction")]
    CoinbaseAfterFirst,

    #[error("coinbase transaction MUST NOT have any JoinSplit descriptions")]
    CoinbaseHasJoinSplit,

    #[error("coinbase transaction MUST NOT have any Spend descriptions")]
    CoinbaseHasSpend,

    #[error("coinbase transaction MUST NOT have any Output descriptions pre-Heartwood")]
    CoinbaseHasOutputPreHeartwood,

    #[error("coinbase transaction MUST NOT have the EnableSpendsOrchard flag set")]
    CoinbaseHasEnableSpendsOrchard,

    #[error("coinbase transaction Sapling or Orchard outputs MUST be decryptable with an all-zero outgoing viewing key")]
    CoinbaseOutputsNotDecryptable,

    #[error("coinbase inputs MUST NOT exist in mempool")]
    CoinbaseInMempool,

    #[error("non-coinbase transactions MUST NOT have coinbase inputs")]
    NonCoinbaseHasCoinbaseInput,

    #[error("transaction is locked until after block height {}", _0.0)]
    LockedUntilAfterBlockHeight(block::Height),

    #[error("transaction is locked until after block time {0}")]
    #[cfg_attr(any(test, feature = "proptest-impl"), proptest(skip))]
    LockedUntilAfterBlockTime(DateTime<Utc>),

    #[error(
        "coinbase expiry {expiry_height:?} must be the same as the block {block_height:?} \
         after NU5 activation, failing transaction: {transaction_hash:?}"
    )]
    CoinbaseExpiryBlockHeight {
        expiry_height: Option<zebra_chain::block::Height>,
        block_height: zebra_chain::block::Height,
        transaction_hash: zebra_chain::transaction::Hash,
    },

    #[error(
        "expiry {expiry_height:?} must be less than the maximum {MAX_EXPIRY_HEIGHT:?} \
         coinbase: {is_coinbase}, block: {block_height:?}, failing transaction: {transaction_hash:?}"
    )]
    MaximumExpiryHeight {
        expiry_height: zebra_chain::block::Height,
        is_coinbase: bool,
        block_height: zebra_chain::block::Height,
        transaction_hash: zebra_chain::transaction::Hash,
    },

    #[error(
        "transaction must not be mined at a block {block_height:?} \
         greater than its expiry {expiry_height:?}, failing transaction {transaction_hash:?}"
    )]
    ExpiredTransaction {
        expiry_height: zebra_chain::block::Height,
        block_height: zebra_chain::block::Height,
        transaction_hash: zebra_chain::transaction::Hash,
    },

    #[error("coinbase transaction failed subsidy validation")]
    #[cfg_attr(any(test, feature = "proptest-impl"), proptest(skip))]
    Subsidy(#[from] SubsidyError),

    #[error("transaction version number MUST be >= 4")]
    WrongVersion,

    #[error("transaction version {0} not supported by the network upgrade {1:?}")]
    UnsupportedByNetworkUpgrade(u32, zebra_chain::parameters::NetworkUpgrade),

    #[error("must have at least one input: transparent, shielded spend, or joinsplit")]
    NoInputs,

    #[error("must have at least one output: transparent, shielded output, or joinsplit")]
    NoOutputs,

    #[error("if there are no Spends or Outputs, the value balance MUST be 0.")]
    BadBalance,

    #[error("could not verify a transparent script")]
    #[cfg_attr(any(test, feature = "proptest-impl"), proptest(skip))]
    Script(#[from] zebra_script::Error),

    #[error("spend description cv and rk MUST NOT be of small order")]
    SmallOrder,

    // XXX: the underlying error is bellman::VerificationError, but it does not implement
    // Arbitrary as required here.
    #[error("spend proof MUST be valid given a primary input formed from the other fields except spendAuthSig")]
    Groth16(String),

    // XXX: the underlying error is io::Error, but it does not implement Clone as required here.
    #[error("Groth16 proof is malformed")]
    MalformedGroth16(String),

    #[error(
        "Sprout joinSplitSig MUST represent a valid signature under joinSplitPubKey of dataToBeSigned"
    )]
    #[cfg_attr(any(test, feature = "proptest-impl"), proptest(skip))]
    Ed25519(#[from] zebra_chain::primitives::ed25519::Error),

    #[error("Sapling bindingSig MUST represent a valid signature under the transaction binding validating key bvk of SigHash")]
    #[cfg_attr(any(test, feature = "proptest-impl"), proptest(skip))]
    RedJubjub(zebra_chain::primitives::redjubjub::Error),

    #[error("Orchard bindingSig MUST represent a valid signature under the transaction binding validating key bvk of SigHash")]
    #[cfg_attr(any(test, feature = "proptest-impl"), proptest(skip))]
    RedPallas(zebra_chain::primitives::redpallas::Error),

    // temporary error type until #1186 is fixed
    #[error("Downcast from BoxError to redjubjub::Error failed")]
    InternalDowncastError(String),

    #[error("either vpub_old or vpub_new must be zero")]
    BothVPubsNonZero,

    #[error("adding to the sprout pool is disabled after Canopy")]
    DisabledAddToSproutPool,

    #[error("could not calculate the transaction fee")]
    IncorrectFee,

    #[error("transparent double-spend: {_0:?} is spent twice")]
    DuplicateTransparentSpend(transparent::OutPoint),

    #[error("sprout double-spend: duplicate nullifier: {_0:?}")]
    DuplicateSproutNullifier(sprout::Nullifier),

    #[error("sapling double-spend: duplicate nullifier: {_0:?}")]
    DuplicateSaplingNullifier(sapling::Nullifier),

    #[error("orchard double-spend: duplicate nullifier: {_0:?}")]
    DuplicateOrchardNullifier(orchard::Nullifier),

    #[error("must have at least one active orchard flag")]
    NotEnoughFlags,
}

impl From<BoxError> for TransactionError {
    fn from(mut err: BoxError) -> Self {
        // TODO: handle redpallas::Error, ScriptInvalid, InvalidSignature
        match err.downcast::<zebra_chain::primitives::redjubjub::Error>() {
            Ok(e) => return TransactionError::RedJubjub(*e),
            Err(e) => err = e,
        }

        // buffered transaction verifier service error
        match err.downcast::<TransactionError>() {
            Ok(e) => return *e,
            Err(e) => err = e,
        }

        TransactionError::InternalDowncastError(format!(
            "downcast to known transaction error type failed, original error: {:?}",
            err,
        ))
    }
}

impl From<SubsidyError> for BlockError {
    fn from(err: SubsidyError) -> BlockError {
        BlockError::Transaction(TransactionError::Subsidy(err))
    }
}

#[derive(Error, Clone, Debug, PartialEq, Eq)]
pub enum BlockError {
    #[error("block contains invalid transactions")]
    Transaction(#[from] TransactionError),

    #[error("block has no transactions")]
    NoTransactions,

    #[error("block has mismatched merkle root")]
    BadMerkleRoot {
        actual: zebra_chain::block::merkle::Root,
        expected: zebra_chain::block::merkle::Root,
    },

    #[error("block contains duplicate transactions")]
    DuplicateTransaction,

    #[error("block {0:?} is already in the chain at depth {1:?}")]
    AlreadyInChain(zebra_chain::block::Hash, u32),

    #[error("invalid block {0:?}: missing block height")]
    MissingHeight(zebra_chain::block::Hash),

    #[error("invalid block height {0:?} in {1:?}: greater than the maximum height {2:?}")]
    MaxHeight(
        zebra_chain::block::Height,
        zebra_chain::block::Hash,
        zebra_chain::block::Height,
    ),

    #[error("invalid difficulty threshold in block header {0:?} {1:?}")]
    InvalidDifficulty(zebra_chain::block::Height, zebra_chain::block::Hash),

    #[error("block {0:?} has a difficulty threshold {2:?} that is easier than the {3:?} difficulty limit {4:?}, hash: {1:?}")]
    TargetDifficultyLimit(
        zebra_chain::block::Height,
        zebra_chain::block::Hash,
        zebra_chain::work::difficulty::ExpandedDifficulty,
        zebra_chain::parameters::Network,
        zebra_chain::work::difficulty::ExpandedDifficulty,
    ),

    #[error(
        "block {0:?} on {3:?} has a hash {1:?} that is easier than its difficulty threshold {2:?}"
    )]
    DifficultyFilter(
        zebra_chain::block::Height,
        zebra_chain::block::Hash,
        zebra_chain::work::difficulty::ExpandedDifficulty,
        zebra_chain::parameters::Network,
    ),

    #[error("transaction has wrong consensus branch id for block network upgrade")]
    WrongTransactionConsensusBranchId,

    #[error(
        "block {height:?} {hash:?} has {legacy_sigop_count} legacy transparent signature operations, \
         but the limit is {MAX_BLOCK_SIGOPS}"
    )]
    TooManyTransparentSignatureOperations {
        height: zebra_chain::block::Height,
        hash: zebra_chain::block::Hash,
        legacy_sigop_count: u64,
    },

    #[error("summing miner fees for block {height:?} {hash:?} failed: {source:?}")]
    SummingMinerFees {
        height: zebra_chain::block::Height,
        hash: zebra_chain::block::Hash,
        source: amount::Error,
    },
}

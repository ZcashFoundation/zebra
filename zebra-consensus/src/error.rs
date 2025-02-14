//! Errors that can occur when checking consensus rules.
//!
//! Each error variant corresponds to a consensus rule, so enumerating
//! all possible verification failures enumerates the consensus rules we
//! implement, and ensures that we don't reject blocks or transactions
//! for a non-enumerated reason.

use chrono::{DateTime, Utc};
use thiserror::Error;

use zebra_chain::{
    amount, block, orchard, sapling, sprout,
    transparent::{self, MIN_TRANSPARENT_COINBASE_MATURITY},
};
use zebra_state::ValidateContextError;

use crate::{block::MAX_BLOCK_SIGOPS, BoxError};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// Workaround for format string identifier rules.
const MAX_EXPIRY_HEIGHT: block::Height = block::Height::MAX_EXPIRY_HEIGHT;

/// Block subsidy errors.
#[derive(Error, Clone, Debug, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum SubsidyError {
    #[error("no coinbase transaction in block")]
    NoCoinbase,

    #[error("funding stream expected output not found")]
    FundingStreamNotFound,

    #[error("miner fees are invalid")]
    InvalidMinerFees,

    #[error("a sum of amounts overflowed")]
    SumOverflow,

    #[error("unsupported height")]
    UnsupportedHeight,

    #[error("invalid amount")]
    InvalidAmount(amount::Error),
}

impl From<amount::Error> for SubsidyError {
    fn from(amount: amount::Error) -> Self {
        Self::InvalidAmount(amount)
    }
}

/// Errors for semantic transaction validation.
#[derive(Error, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
#[allow(missing_docs)]
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

    #[error("the tx is not coinbase, but it should be")]
    NotCoinbase,

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

    #[error("coinbase transaction failed subsidy validation: {0}")]
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

    #[error("could not verify a transparent script: {0}")]
    #[cfg_attr(any(test, feature = "proptest-impl"), proptest(skip))]
    Script(#[from] zebra_script::Error),

    #[error("spend description cv and rk MUST NOT be of small order")]
    SmallOrder,

    // TODO: the underlying error is bellman::VerificationError, but it does not implement
    // Arbitrary as required here.
    #[error("spend proof MUST be valid given a primary input formed from the other fields except spendAuthSig: {0}")]
    Groth16(String),

    // TODO: the underlying error is io::Error, but it does not implement Clone as required here.
    #[error("Groth16 proof is malformed: {0}")]
    MalformedGroth16(String),

    #[error(
        "Sprout joinSplitSig MUST represent a valid signature under joinSplitPubKey of dataToBeSigned: {0}"
    )]
    #[cfg_attr(any(test, feature = "proptest-impl"), proptest(skip))]
    Ed25519(#[from] zebra_chain::primitives::ed25519::Error),

    #[error("Sapling bindingSig MUST represent a valid signature under the transaction binding validating key bvk of SigHash: {0}")]
    #[cfg_attr(any(test, feature = "proptest-impl"), proptest(skip))]
    RedJubjub(zebra_chain::primitives::redjubjub::Error),

    #[error("Orchard bindingSig MUST represent a valid signature under the transaction binding validating key bvk of SigHash: {0}")]
    #[cfg_attr(any(test, feature = "proptest-impl"), proptest(skip))]
    RedPallas(zebra_chain::primitives::reddsa::Error),

    // temporary error type until #1186 is fixed
    #[error("Downcast from BoxError to redjubjub::Error failed: {0}")]
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

    #[error("could not find a mempool transaction input UTXO in the best chain")]
    TransparentInputNotFound,

    #[error("could not contextually validate transaction on best chain: {0}")]
    #[cfg_attr(any(test, feature = "proptest-impl"), proptest(skip))]
    // This error variant is at least 128 bytes
    ValidateContextError(Box<ValidateContextError>),

    #[error("could not validate mempool transaction lock time on best chain: {0}")]
    #[cfg_attr(any(test, feature = "proptest-impl"), proptest(skip))]
    // TODO: turn this into a typed error
    ValidateMempoolLockTimeError(String),

    #[error(
        "immature transparent coinbase spend: \
        attempt to spend {outpoint:?} at {spend_height:?}, \
        but spends are invalid before {min_spend_height:?}, \
        which is {MIN_TRANSPARENT_COINBASE_MATURITY:?} blocks \
        after it was created at {created_height:?}"
    )]
    #[non_exhaustive]
    ImmatureTransparentCoinbaseSpend {
        outpoint: transparent::OutPoint,
        spend_height: block::Height,
        min_spend_height: block::Height,
        created_height: block::Height,
    },

    #[error(
        "unshielded transparent coinbase spend: {outpoint:?} \
         must be spent in a transaction which only has shielded outputs"
    )]
    #[non_exhaustive]
    UnshieldedTransparentCoinbaseSpend {
        outpoint: transparent::OutPoint,
        min_spend_height: block::Height,
    },

    #[error(
        "failed to verify ZIP-317 transaction rules, transaction was not inserted to mempool: {0}"
    )]
    #[cfg_attr(any(test, feature = "proptest-impl"), proptest(skip))]
    Zip317(#[from] zebra_chain::transaction::zip317::Error),

    #[error("transaction uses an incorrect consensus branch id")]
    WrongConsensusBranchId,

    #[error("wrong tx format: tx version is ≥ 5, but `nConsensusBranchId` is missing")]
    MissingConsensusBranchId,
}

impl From<ValidateContextError> for TransactionError {
    fn from(err: ValidateContextError) -> Self {
        TransactionError::ValidateContextError(Box::new(err))
    }
}

// TODO: use a dedicated variant and From impl for each concrete type, and update callers (#5732)
impl From<BoxError> for TransactionError {
    fn from(mut err: BoxError) -> Self {
        // TODO: handle redpallas::Error, ScriptInvalid, InvalidSignature
        match err.downcast::<zebra_chain::primitives::redjubjub::Error>() {
            Ok(e) => return TransactionError::RedJubjub(*e),
            Err(e) => err = e,
        }

        match err.downcast::<ValidateContextError>() {
            Ok(e) => return (*e).into(),
            Err(e) => err = e,
        }

        // buffered transaction verifier service error
        match err.downcast::<TransactionError>() {
            Ok(e) => return *e,
            Err(e) => err = e,
        }

        TransactionError::InternalDowncastError(format!(
            "downcast to known transaction error type failed, original error: {err:?}",
        ))
    }
}

impl TransactionError {
    /// Returns a suggested misbehaviour score increment for a certain error when
    /// verifying a mempool transaction.
    pub fn mempool_misbehavior_score(&self) -> u32 {
        use TransactionError::*;

        // TODO: Adjust these values based on zcashd (#9258).
        match self {
            ImmatureTransparentCoinbaseSpend { .. }
            | UnshieldedTransparentCoinbaseSpend { .. }
            | CoinbasePosition
            | CoinbaseAfterFirst
            | CoinbaseHasJoinSplit
            | CoinbaseHasSpend
            | CoinbaseHasOutputPreHeartwood
            | CoinbaseHasEnableSpendsOrchard
            | CoinbaseOutputsNotDecryptable
            | CoinbaseInMempool
            | NonCoinbaseHasCoinbaseInput
            | CoinbaseExpiryBlockHeight { .. }
            | IncorrectFee
            | Subsidy(_)
            | WrongVersion
            | NoInputs
            | NoOutputs
            | BadBalance
            | Script(_)
            | SmallOrder
            | Groth16(_)
            | MalformedGroth16(_)
            | Ed25519(_)
            | RedJubjub(_)
            | RedPallas(_)
            | BothVPubsNonZero
            | DisabledAddToSproutPool
            | NotEnoughFlags
            | WrongConsensusBranchId
            | MissingConsensusBranchId => 100,

            _other => 0,
        }
    }
}

#[derive(Error, Clone, Debug, PartialEq, Eq)]
#[allow(missing_docs)]
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

    #[error("block {0:?} is already in present in the state {1:?}")]
    AlreadyInChain(zebra_chain::block::Hash, zebra_state::KnownBlock),

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

    #[error("unexpected error occurred: {0}")]
    Other(String),
}

impl From<SubsidyError> for BlockError {
    fn from(err: SubsidyError) -> BlockError {
        BlockError::Transaction(TransactionError::Subsidy(err))
    }
}

impl BlockError {
    /// Returns `true` if this is definitely a duplicate request.
    /// Some duplicate requests might not be detected, and therefore return `false`.
    pub fn is_duplicate_request(&self) -> bool {
        matches!(self, BlockError::AlreadyInChain(..))
    }

    /// Returns a suggested misbehaviour score increment for a certain error.
    pub(crate) fn misbehavior_score(&self) -> u32 {
        use BlockError::*;

        match self {
            MissingHeight(_)
            | MaxHeight(_, _, _)
            | InvalidDifficulty(_, _)
            | TargetDifficultyLimit(_, _, _, _, _)
            | DifficultyFilter(_, _, _, _) => 100,
            _other => 0,
        }
    }
}

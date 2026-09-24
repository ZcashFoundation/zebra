//! Error types for `ParametersBuilder`.

use std::path::PathBuf;

use thiserror::Error;

/// An error that can occur when building `Parameters` using `ParametersBuilder`.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ParametersBuilderError {
    #[error("cannot use reserved network name '{network_name}' as configured Testnet name, reserved names: {reserved_names:?}")]
    #[non_exhaustive]
    ReservedNetworkName {
        network_name: String,
        reserved_names: Vec<&'static str>,
    },

    #[error("network name {network_name} is too long, must be {max_length} characters or less")]
    #[non_exhaustive]
    NetworkNameTooLong {
        network_name: String,
        max_length: usize,
    },

    #[error("network name must include only alphanumeric characters or '_'")]
    #[non_exhaustive]
    InvalidCharacter,

    #[error("network magic should be distinct from reserved network magics")]
    #[non_exhaustive]
    ReservedNetworkMagic,

    #[error("configured genesis hash must parse")]
    #[non_exhaustive]
    InvalidGenesisHash,

    #[error(
        "activation heights on ParametersBuilder must not be set after setting funding streams"
    )]
    #[non_exhaustive]
    LockFundingStreams,

    #[error("activation height must be valid")]
    #[non_exhaustive]
    InvalidActivationHeight,

    #[error("Height(0) is reserved for the `Genesis` upgrade")]
    #[non_exhaustive]
    InvalidHeightZero,

    #[error("NSM reissuance requires a nonzero height at or after NU7 activation")]
    #[non_exhaustive]
    InvalidNsmReissuanceHeight,

    #[error("halving interval must be positive, give supported halving indices and heights, a nonzero funding stream address period, and a positive NSM coefficient when reissuance is configured")]
    #[non_exhaustive]
    InvalidHalvingInterval,

    #[error("scheduled issuance through the maximum supported height must not exceed MAX_MONEY, including the scheduled genesis subsidy when NU7 seeds the NSM reserve")]
    #[non_exhaustive]
    InvalidSubsidySchedule,

    #[error("network upgrades must be activated in order specified by the protocol")]
    #[non_exhaustive]
    OutOfOrderUpgrades,

    #[error("difficulty limits are valid expanded values")]
    #[non_exhaustive]
    InvaildDifficultyLimits,

    #[error("halving interval on ParametersBuilder must not be set after setting funding streams")]
    #[non_exhaustive]
    HalvingIntervalAfterFundingStreams,

    #[error("checkpoints file format must be valid")]
    #[non_exhaustive]
    InvalidCheckpointsFormat,

    #[error("must parse checkpoints")]
    #[non_exhaustive]
    FailedToParseDefaultCheckpoint,

    #[error("could not read file at configured checkpoints file path: {path_buf:?}")]
    #[non_exhaustive]
    FailedToReadCheckpointFile { path_buf: PathBuf },

    #[error("could not parse checkpoints at the provided path: {path_buf:?}, err: {err}")]
    #[non_exhaustive]
    FailedToParseCheckpointFile { path_buf: PathBuf, err: String },

    #[error("configured checkpoints must be valid")]
    #[non_exhaustive]
    InvalidCustomCheckpoints,

    #[error("first checkpoint hash must match genesis hash")]
    #[non_exhaustive]
    CheckpointGenesisMismatch,

    #[error(
        "checkpoints must be provided for block heights below the mandatory checkpoint height"
    )]
    #[non_exhaustive]
    InsufficientCheckpointCoverage,
}

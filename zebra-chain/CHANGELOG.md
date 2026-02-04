# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `BoundedVec` dependency
- `parameters::network::subsidy::constants` module.

### Changed

- `AtLeastOne` is now a type alias for `BoundedVec`.

### Removed

- All constants from `parameters::network::subsidy`.

## [4.0.0] - 2026-01-21

### Breaking Changes

All `ParametersBuilder` methods and `Parameters::new_regtest()` now return `Result` types instead of `Self`:

- `Parameters::new_regtest()` - Returns `Result<Self, ParametersBuilderError>`
- `ParametersBuilder::clear_checkpoints()` - Returns `Result<Self, ParametersBuilderError>`
- `ParametersBuilder::to_network()` - Returns `Result<Network, ParametersBuilderError>`
- `ParametersBuilder::with_activation_heights()` - Returns `Result<Self, ParametersBuilderError>`
- `ParametersBuilder::with_checkpoints()` - Returns `Result<Self, ParametersBuilderError>`
- `ParametersBuilder::with_genesis_hash()` - Returns `Result<Self, ParametersBuilderError>`
- `ParametersBuilder::with_halving_interval()` - Returns `Result<Self, ParametersBuilderError>`
- `ParametersBuilder::with_network_magic()` - Returns `Result<Self, ParametersBuilderError>`
- `ParametersBuilder::with_network_name()` - Returns `Result<Self, ParametersBuilderError>`
- `ParametersBuilder::with_target_difficulty_limit()` - Returns `Result<Self, ParametersBuilderError>`

**Migration:**

- Chain builder calls with `?` operator: `.with_network_name("test")?`
- Or use `.expect()` if errors are unexpected: `.with_network_name("test").expect("valid name")`


## [3.1.0] - 2025-11-28

### Added

- Added `Output::is_dust()`
- Added `ONE_THIRD_DUST_THRESHOLD_RATE`

## [3.0.1] - 2025-11-17

### Added

- Added `From<SerializationError>` implementation for `std::io::Error`
- Added `InvalidMinFee` error variant to `zebra_chain::transaction::zip317::Error`
- Added `Transaction::zip233_amount()` method

## [3.0.0] - 2025-10-15

In this release we removed a significant amount of Sapling-related code in favor of upstream implementations.
These changes break the public API and may require updates in downstream crates. ([#9828](https://github.com/ZcashFoundation/zebra/issues/9828))

### Breaking Changes

- The `ValueCommitment` type no longer derives `Copy`.
- `zebra-chain::Errors` has new variants.
- ` ValueCommitment::new` and `ValueCommitment::randomized` methods were removed.
- Constant `NU6_1_ACTIVATION_HEIGHT_TESTNET` was removed as is now part of `activation_heights` module.
- Structs `sapling::NoteCommitment`, `sapling::NotSmallOrderValueCommitment` and `sapling::tree::Node` were
  removed.

### Added

- Added `{sapling,orchard}::Root::bytes_in_display_order()`
- Added `bytes_in_display_order()` for multiple `sprout` types,
  as well for `orchard::tree::Root` and `Halo2Proof`.
- Added `CHAIN_HISTORY_ACTIVATION_RESERVED` as an export from the `block` module.
- Added `extend_funding_stream_addresses_as_required` field to `RegtestParameters` struct
- Added `extend_funding_stream_addresses_as_required` field to `DTestnetParameters` struct

### Removed

- Removed call to `check_funding_stream_address_period` in `convert_with_default()`

## [2.0.0] - 2025-08-07

Support for NU6.1 testnet activation; added testnet activation height for NU6.1.

### Breaking Changes

- Renamed `legacy_sigop_count` to `sigops` in `VerifiedUnminedTx`
- Added `SubsidyError::OneTimeLockboxDisbursementNotFound` enum variant
- Removed `zebra_chain::parameters::subsidy::output_amounts()`
- Refactored `{pre, post}_nu6_funding_streams` fields in `testnet::{Parameters, ParametersBuilder}` into one `BTreeMap``funding_streams` field
- Removed `{PRE, POST}_NU6_FUNDING_STREAMS_{MAINNET, TESTNET}`;
  they're now part of `FUNDING_STREAMS_{MAINNET, TESTNET}`.
- Removed `ConfiguredFundingStreams::empty()`
- Changed `ConfiguredFundingStreams::convert_with_default()` to take
  an `Option<FundingStreams>`.

### Added

- Added `new_from_zec()`, `new()`, `div_exact()` methods for `Amount<NonNegative>`
- Added `checked_sub()` method for `Amount`
- Added `DeferredPoolBalanceChange` newtype wrapper around `Amount`s representing deferred pool balance changes
- Added `Network::lockbox_disbursement_total_amount()` and
  `Network::lockbox_disbursements()` methods
- Added `NU6_1_LOCKBOX_DISBURSEMENTS_{MAINNET, TESTNET}`, `POST_NU6_1_FUNDING_STREAM_FPF_ADDRESSES_TESTNET`, and `NU6_1_ACTIVATION_HEIGHT_TESTNET` constants
- Added `ConfiguredLockboxDisbursement`
- Added `ParametersBuilder::{with_funding_streams(), with_lockbox_disbursements()}` and
  `Parameters::{lockbox_disbursement_total_amount(), lockbox_disbursements()}` methods

## [1.0.0] - 2025-07-11

First "stable" release. However, be advised that the API may still greatly
change so major version bumps can be common.

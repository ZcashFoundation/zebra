# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
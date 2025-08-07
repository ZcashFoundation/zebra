# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [2.0.0] - 2025-08-07

Support for NU6.1 testnet activation.

### Breaking Changes

- Renamed `legacy_sigop_count` to `sigops` in `VerifiedUnminedTx`
- Added `SubsidyError::OneTimeLockboxDisbursementNotFound` enum variant
- Removed `zebra_chain::parameters::subsidy::output_amounts()`
- Removed `pre_nu6_funding_streams` and `post_nu6_funding_streams` from
  `Parameters`, `Network` and `ParametersBuilder`. Use `funding_streams`
  instead.
- Removed `PRE_NU6_FUNDING_STREAMS_MAINNET`, `POST_NU6_FUNDING_STREAMS_TESTNET`,
  `POST_NU6_FUNDING_STREAMS_MAINNET`, `PRE_NU6_FUNDING_STREAMS_TESTNET`;
  they're now part of `FUNDING_STREAMS_MAINNET/TESTNET`.
- Removed `ConfiguredFundingStreams::empty()`
- Changed `ConfiguredFundingStreams::convert_with_default()` to take
  an `Option<FundingStreams>`.

### Added

- Added `new_from_zec()`, `new()`, `div_exact()` to `Amount<NonNegative>`
- Added `checked_sub()` to `Amount`
- Added `DeferredPoolBalanceChange`
- Added `Network::lockbox_disbursement_total_amount()`,
  `Network::lockbox_disbursements()`
- Added `NU6_1_LOCKBOX_DISBURSEMENTS_MAINNET`,
  `NU6_1_LOCKBOX_DISBURSEMENTS_TESTNET`,
  `POST_NU6_1_FUNDING_STREAM_FPF_ADDRESSES_TESTNET`
- Added `ConfiguredLockboxDisbursement`
- Added `ParametersBuilder::with_funding_streams()/with_lockbox_disbursements()`
- Added
  `Parameters::lockbox_disbursement_total_amount()/lockbox_disbursements()`
- Added `NU6_1_ACTIVATION_HEIGHT_TESTNET`

## [1.0.0] - 2025-07-11

First "stable" release. However, be advised that the API may still greatly
change so major version bumps can be common.
# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `AT_OR_NEAR_TIP_THRESHOLD` constant and `ChainTip::is_at_or_near_network_tip()`
  method for determining whether the node is within 5 blocks of the estimated network tip
  ([#10732](https://github.com/ZcashFoundation/zebra/pull/10732))

## [11.0.0] - 2026-07-02

### Added

- `parameters::NetworkUpgrade::Nu6_3`
- `parameters::constants::activation_heights::testnet::NU6_3`
- `parameters::testnet::ConfiguredActivationHeights::nu6_3`
- `parameters::testnet::RegtestParameters::should_allow_unshielded_coinbase_spends`:
  optional override for whether Regtest allows coinbase outputs to be spent into
  transparent outputs. Defaults to allowing them, and does not affect
  `Network::is_regtest()`.
- `ironwood` module
- `impl {ZcashSerialize, ZcashDeserialize} for Option<ironwood::ShieldedData>`
- `impl From<ironwood::Nullifier> for [u8; 32]`
- `orchard::shielded_data::Flags::ENABLE_CROSS_ADDRESS`
- `orchard::shielded_data::FlagsV6` (re-exported as `orchard::FlagsV6`).
- `orchard::shielded_data::ShieldedDataV6::{new, data, data_mut, into_inner}` (re-exported as
  `orchard::ShieldedDataV6`).
- `impl ZcashDeserialize for orchard::shielded_data::FlagsV6`
- `impl {ZcashSerialize, ZcashDeserialize} for Option<orchard::shielded_data::ShieldedDataV6>`
- `impl From<orchard::shielded_data::FlagsV6> for orchard::shielded_data::Flags`
- `block::Block::{ironwood_note_commitments, ironwood_nullifiers, ironwood_transactions_count}`
- `transaction::Transaction`:
  - `V6 { network_upgrade, lock_time, expiry_height, inputs, outputs, sapling_shielded_data,
    orchard_shielded_data, ironwood_shielded_data }`
  - `ironwood_actions`
  - `ironwood_flags`
  - `ironwood_shielded_data`
  - `ironwood_note_commitments`
  - `ironwood_nullifiers`
  - `ironwood_value_balance`
  - `has_ironwood_shielded_data`
  - `has_enough_ironwood_flags`
- `transaction::SigHasher::ironwood_bundle`
- `transaction::arbitrary::{fake_v6_orchard_shielded_data, fake_v6_transaction}`
- `value_balance::ValueBalance::{from_ironwood_amount, ironwood_amount, set_ironwood_value_balance}`
- `value_balance::ValueBalanceError::Ironwood`
- `parallel::tree::NoteCommitmentTrees`:
  - `ironwood`
  - `ironwood_subtree`
  - `update_ironwood_note_commitment_tree`
- `parallel::tree::NoteCommitmentTreeError::Ironwood`
- `primitives::zcash_history::V3` (the ZIP-221 Ironwood history node).
- `impl Version for zcash_history::version::V3`
- `primitives::zcash_history::Entry::from_raw_bytes_padded`
- `primitives::zcash_history::BlockCommitmentTreeRoots`, grouping a block's Sapling,
  Orchard, and Ironwood note commitment tree roots.

### Changed

- Migrated to `zcash_primitives 0.29.0-pre.0` (and the rest of the librustzcash NU6.3
  pre-release wave: `orchard 0.15.0-pre.1`, `zcash_address 0.13.0-pre.0`,
  `zcash_history 0.5.0-pre.0`, `zcash_protocol 0.10.0-pre.0`, `zcash_transparent 0.9.0-pre.0`).
- Migrated to `strum 0.27`.
- The following history-tree functions now take a
  `primitives::zcash_history::BlockCommitmentTreeRoots` struct grouping the Sapling,
  Orchard, and Ironwood roots by name, instead of separate positional root parameters:
  - `history_tree::HistoryTree::{from_block, push}`
  - `history_tree::NonEmptyHistoryTree::{from_block, push, try_extend}`
  - `primitives::zcash_history::Tree::{append_leaf, new_from_block}`
  - `primitives::zcash_history::Version::block_to_history_node`
- `value_balance::ValueBalance<NonNegative>::to_bytes` now returns `[u8; 48]`
  (was `[u8; 40]`), to include the Ironwood pool balance.

### Removed

- `transaction::Transaction::zip233_amount` (the abandoned ZIP-233 burn amount).

## [10.1.0] - 2026-06-18

### Added

- `parameters::constants::MAX_BLOCK_REORG_HEIGHT`: the maximum chain reorganisation height (1000).

## [10.0.0] - 2026-06-10

### Breaking Changes

- `Block::chain_value_pool_change()`: the `deferred_pool_balance_change` parameter type
  changed from `Option<DeferredPoolBalanceChange>` to `DeferredPoolBalanceChange`.

### Changed

- Updated mainnet and testnet checkpoints.

## [9.0.0] - 2026-06-02

### Added

- `NetworkUpgrade::Nu6_2` (consensus branch id `0x5437f330`), with activation heights
  3,364,600 on Mainnet and 4,052,000 on Testnet.
- `OrchardShieldedData::proof_size_is_canonical()`.
- `Network::orchard_canonical_proof_size_rule_active()` and
  `Network::is_orchard_temporarily_disabled()`.
- A configurable NU6.2 activation height for Testnets (`ConfiguredActivationHeights::nu6_2`).

### Changed

- The default Testnet's temporary Orchard-disabling soft-fork height now defaults to
  4,048,500; Regtest leaves it unset.

## [8.0.0] - 2026-05-28

### Removed

- `block::Height::coinbase_zcash_serialized_size()`
- `transaction`:
  - `builder` module
  - `Transaction::new_v4_coinbase()` and `new_v5_coinbase()`
- `transparent`:
  - `Input::new_coinbase()` and `extra_coinbase_data()`
  - `CoinbaseData` struct and its impls
  - `EXTRA_ZEBRA_COINBASE_DATA`, `GENESIS_COINBASE_DATA`, `MAX_COINBASE_DATA_LEN`,
    `MAX_COINBASE_HEIGHT_DATA_LEN` constants

### Changed

- `transparent::Input::Coinbase`:
  - `data` field type changed from `CoinbaseData` to `Vec<u8>`
  - `data` now stores only miner data (without height encoding)
- `block::Hash::max_allocation()` now returns `MAX_BLOCK_LOCATOR_LENGTH` (`101`,
  matching Bitcoin Core's `MAX_LOCATOR_SZ`); previously derived from
  `MAX_PROTOCOL_MESSAGE_LEN` (~65,535).
- `block::CountedHeader::max_allocation()` now returns `MAX_HEADERS_PER_MESSAGE`
  (`160`); previously ~1,409. Mitigates upfront preallocation by a
  post-handshake peer on `getblocks`/`getheaders` (CWE-770; same fix shape as
  [GHSA-xr93-pcq3-pxf8](https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-xr93-pcq3-pxf8)).
- `serialization::zcash_deserialize_external_count` now caps the initial
  `Vec::with_capacity` reservation at `MAX_INITIAL_ALLOCATION = 1024` so a
  peer-supplied `CompactSize` cannot force a large allocation before any
  element bytes are read; the `Vec` grows naturally via `push()`. Complements
  the per-type `max_allocation()` caps (CWE-770).

### Added

- `block::MAX_BLOCK_LOCATOR_LENGTH: u64 = 101`.
- `block::Height`:
  - `impl From<block::Height> for i64`
  - `impl From<&block::Height> for i64`
  - `impl TryFrom<i64> for block::Height`
- `transparent`:
  - `Input::miner_data()`
  - `Input::coinbase_script()`
  - `impl TryFrom<transparent::Address> for zcash_transparent::address::TransparentAddress`
  - `derive(Copy)` on `transparent::Address`
- `transaction`:
  - `impl TryFrom<&[u8]> for AuthDigest`
  - `impl AsRef<[u8; 32]> for Hash`
  - `impl From<&[u8; 32]> for Hash`
- `serialization`:
  - `SerializationError::{Num, Opcode, Script}` variants
  - `impl ZcashSerialize for u8`

### Fixed

- `Block::chain_value_pool_change()` now propagates per-transaction
  `ValueBalanceError`s instead of silently dropping them via `flat_map(Result)`
  ([#10585](https://github.com/ZcashFoundation/zebra/issues/10585)).

## [7.0.0] - 2026-05-01

### Added

- `serialization::MAX_HEADERS_PER_MESSAGE: usize`.
- `transaction::VerifiedUnminedTx`:
  - `p2sh_sigop_count: u32`.
  - `block_sigop_count(&self) -> u32`.

### Changed

- Migrated to `zcash_primitives 0.27` (and the rest of the librustzcash 2026-04
  release wave), which replaces the yanked `core2` dependency with `corez`.
- `transaction::VerifiedUnminedTx::new` now takes an additional
  `p2sh_sigop_count: u32` parameter.

## [6.0.2] - 2026-04-17

This release fixes an important security issue:

- [CVE-2026-XXXXX: rk Identity Point Panic in Transaction Verification](https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-452v-w3gx-72wg)

The impact of the issue for crate users will depend on the particular usage;
if you use it as a building block for a consensus node, you should update.

## [6.0.1] - 2026-03-26

This release fixes an important security issue:

- [CVE-2026-34202: Remote Denial of Service via Crafted V5 Transactions](https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-qp6f-w4r3-h8wg)

The impact of the issue for crate users will depend on the particular usage;
if you use zebra-chain to parse untrusted transactions, a particularly crafted
transaction will raise a panic which will crash your application; you should
update.

### Fixed

- Fixed miner subsidy computation.

## [6.0.0] - 2026-03-12

### Breaking Changes

- Removed `zebra_chain::diagnostic::CodeTimer::finish` — replaced by `finish_desc` and `finish_inner`
- Removed `SubsidyError::SumOverflow` variant — replaced by `SubsidyError::Overflow` and `SubsidyError::Underflow`
- Removed `zebra_chain::parameters::subsidy::num_halvings` — replaced by `halving`
- Removed `VerifiedUnminedTx::sigops` field — replaced by `legacy_sigop_count`
- Removed `transparent::Output::new_coinbase` — replaced by `Output::new`
- Changed `block_subsidy` parameter renamed from `network` to `net` (no behavioral change)
- Changed `VerifiedUnminedTx::new` — added required `spent_outputs: Arc<Vec<Output>>` parameter

### Added

- Added `Amount::is_zero(&self) -> bool`
- Added `From<Height> for u32` and `From<Height> for u64` conversions
- Added `CodeTimer::start_desc(description: &'static str) -> Self`
- Added `CodeTimer::finish_desc(self, description: &'static str)`
- Added `CodeTimer::finish_inner` with optional file/line and description
- Added `SubsidyError::FoundersRewardNotFound`, `SubsidyError::Overflow`, `SubsidyError::Underflow` variants
- Added `founders_reward(net, height) -> Amount` — returns the founders reward amount for a given height
- Added `founders_reward_address(net, height) -> Option<Address>` — returns the founders reward address for a given height
- Added `halving(height, network) -> u32` — replaces removed `num_halvings`
- Added `Network::founder_address_list(&self) -> &[&str]`
- Added `NetworkUpgradeIter` struct
- Added `VerifiedUnminedTx::legacy_sigop_count: u32` field
- Added `VerifiedUnminedTx::spent_outputs: Arc<Vec<Output>>` field
- Added `transparent::Output::new(amount, lock_script) -> Output` — replaces removed `new_coinbase`

## [5.0.0] - 2026-02-05

### Breaking Changes

- `AtLeastOne<T>` is now a type alias for `BoundedVec<T, 1, { usize::MAX }>`.

### Added

- `BoundedVec` re-export.
- `OrchardActions` trait with `actions()` method.
- `ConfiguredFundingStreamRecipient::new_for()` method.
- `strum`, `bounded-vec` dependencies.

### Changed

- `parameters/network_upgrade/NetworkUpgrade` now derives `strum::EnumIter`

### Removed

- All constants from `parameters::network::subsidy`.
- `AtLeastOne<T>` struct (replaced with type alias to `BoundedVec`).

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
- `ValueCommitment::new` and `ValueCommitment::randomized` methods were removed.
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

# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org).

## [14.0.1] - 2026-10-08

### Breaking Changes

- Updated `zcash_primitives` to 0.31.0-pre.1, `zcash_address` to 0.14.0-pre.1 and `zcash_transparent` to 0.11.0-pre.1. Their types appear in the public API. `zcash_protocol` remains at 0.11.0-pre.0. ([#11614](https://github.com/ZcashFoundation/zebra/pull/11614))

### Security

- Bound the upfront `Vec` reservation in `zcash_deserialize_bytes_external_count` (and therefore `zcash_deserialize_string_external_count`, which delegates to it) so a peer-supplied byte count cannot force a large allocation before any payload byte is read. The buffer now grows incrementally as real bytes arrive, mirroring the element-path cap added in PR #10563. CWE-770 ([#10572](https://github.com/ZcashFoundation/zebra/issues/10572)).

## [14.0.0] - 2026-10-01

### Breaking Changes

- `parameters::subsidy::{scheduled_block_subsidy, block_subsidy_with_parent_pools}` expose scheduled and parent-dependent subsidies. Adds NSM issuance, including `ValueBalance::{nsm_amount, set_nsm_amount}`. Update `parameters::testnet::RegtestParameters` literals for the new `initial_nsm_value_balance` and `nsm_reissuance_height` fields. The explicit initial reserve is credited at NU7 activation and excluded from issued supply. Omitting the reissuance height derives ZIP 237's scheduled-issuance crossover; short custom schedules may have none. `parameters::testnet::Parameters::configured_nsm_reissuance_height` returns only the explicit setting, unlike the effective `nsm_reissuance_height` accessor on `parameters::{Network, testnet::Parameters}`. Use `parameters::testnet::ParametersBuilder::with_nsm_reissuance_height` for explicit overrides and `parameters::subsidy::nsm_reissuance_is_active` to check activation. From the NSM reissuance height, context-free `parameters::subsidy::block_subsidy` returns `SubsidyError::ParentChainValuePoolsRequired`; use the parent-aware API for the total subsidy. Update exhaustive matches for `parameters::subsidy::SubsidyError::{Other, ParentChainValuePoolsRequired}` and `value_balance::ValueBalanceError::Nsm` ([#11454](https://github.com/ZcashFoundation/zebra/pull/11454), [#11530](https://github.com/ZcashFoundation/zebra/pull/11530)).
- [ZIP 218](https://zips.z.cash/zip-0218) parameters for NU7: a 25-second target block spacing, a 102-block difficulty averaging window, a 5,040,000-block halving interval, and a block subsidy divided by a further factor of 3, so issuance per unit of wall clock time is unchanged. Implementers of `parameters::subsidy::ParameterSubsidy` must provide `post_nu7_halving_interval()` and `nu7_activation_height() -> Option<Height>`. `NetworkUpgrade::averaging_window()` and `averaging_window_for_height()` replace the `POW_AVERAGING_WINDOW` constant for consensus checks. Adds `parameters::{POST_NU7_POW_TARGET_SPACING, NU7_POW_TARGET_SPACING_RATIO, POST_NU7_POW_AVERAGING_WINDOW, MAX_POW_AVERAGING_WINDOW}`. ([#11529](https://github.com/ZcashFoundation/zebra/pull/11529))
- `Block::chain_value_pool_change` now requires the network and parent chain value pools. `Block::chain_value_pool_change_and_fees` returns the same accounting delta and optional aggregate fees in one traversal; neither method validates the coinbase payout. `ValueBalance` serialization uses a fixed 56-byte representation including the NSM balance, even when zero. `transparent::utxos_from_ordered_utxos` accepts an iterator of owned entries, avoiding an intermediate map ([#11530](https://github.com/ZcashFoundation/zebra/pull/11530)).
- `parameters::testnet::ParametersBuilder::extend_funding_streams` returns `Result<Self, ParametersBuilderError>` instead of `Self`. Builder validation rejects unsupported halving schedules, issuance above the monetary cap, zero configured funding address periods, and insufficient recipient addresses. `to_network` returns an error rather than panicking on address shortages; explicitly extend addresses when required. `with_halving_interval` requires an interval in `1..=Height::MAX` and the first halving must be representable. ([#11529](https://github.com/ZcashFoundation/zebra/pull/11529))
- `chain_tip::ChainTip::is_at_or_near_network_tip` now accepts the current `chrono::DateTime<Utc>` instead of a network reference and compares the tip age. Replace `chain_tip::AT_OR_NEAR_TIP_THRESHOLD` with `chain_tip::AT_OR_NEAR_TIP_MAX_AGE`, a `chrono::Duration`, to preserve the freshness allowance across NU7 ([#11529](https://github.com/ZcashFoundation/zebra/pull/11529)).
- `parameters::subsidy::funding_stream_address_period` now returns signed `HeightDiff` and floors negative periods; subtract periods before converting to an unsigned funding-address index. ZIP 207 triples the remaining partial address period at NU7, then uses 105,000-block periods on the public schedule. Canonical ZIP 214 revision-2 streams active at NU7 end at the adjusted third halving; expired streams are not reactivated and explicit custom ranges are preserved. ([#11529](https://github.com/ZcashFoundation/zebra/pull/11529))
- Exclude unspendable genesis transparent outputs from chain value-pool accounting. Existing custom-network state with nonzero genesis outputs must be rebuilt; public-network genesis outputs are zero and their issued balances are unchanged ([#11530](https://github.com/ZcashFoundation/zebra/pull/11530)).
- `parameters::subsidy::{MAINNET_INITIAL_NSM_VALUE_BALANCE, TESTNET_INITIAL_NSM_VALUE_BALANCE}` specify the historical public reserve seeds: 36,858,445,520 and 55,768,414,957 zatoshis. Testnets using public Testnet magic inherit its seed when omitted, regardless of network name or checkpoints; other Testnets and Regtest default to zero. `parameters::testnet::Parameters::configured_initial_nsm_value_balance` returns only the explicit setting ([#11530](https://github.com/ZcashFoundation/zebra/pull/11530)).
- `parameters::testnet::{ParametersBuilder::to_network, Parameters::new_regtest}` reject overlapping nonempty funding-stream height ranges after inherited NU7 defaults are applied. Update custom parameters to use disjoint ranges; adjacent and empty ranges are accepted, including empty recipient lists when automatically extending an empty range ([#11554](https://github.com/ZcashFoundation/zebra/pull/11554)).
- `parameters::testnet::{ParametersBuilder::to_network, Parameters::new_regtest}` reject TEX addresses for non-deferred funding-stream recipients. Configure P2SH or P2PKH recipients instead ([#11554](https://github.com/ZcashFoundation/zebra/pull/11554)).
- Updated `zcash_primitives` to 0.31.0-pre.0, `zcash_protocol` to 0.11.0-pre.0, `zcash_address` to 0.14.0-pre.0 and `zcash_transparent` to 0.11.0-pre.0 for NU7, along with `orchard` 0.16, `sapling-crypto` 0.9, `halo2_proofs` 0.4, `reddsa` 0.6, `redjubjub` 0.9, `jubjub` 0.11, `incrementalmerkletree` 0.9, `zcash_note_encryption` 0.5, `zcash_script` 0.6 and `secp256k1` 0.33. Their types appear in the public API, including the `primitives::{reddsa, redjubjub}` and `transaction::TxVersion` re-exports, `Transaction::orchard_flags`, and the `transaction::compat` conversions ([#11559](https://github.com/ZcashFoundation/zebra/pull/11559)).
- Updated `ed25519-zebra` to 5.0.0, `x25519-dalek` to 3 and `rand_core` to 0.10. The first two are re-exported as `primitives::{ed25519, x25519}`, and `orchard::Diversifier::new`, `orchard::Note::new`, `orchard::ValueCommitment::randomized` and `sapling::Diversifier::new` now take a `rand_core` 0.10 `Rng + CryptoRng` ([#11559](https://github.com/ZcashFoundation/zebra/pull/11559)).

### Added

- `parameters::subsidy::{subsidy_is_valid, miner_fees_are_valid, funding_stream_address}` and `CoinbaseTransactionError`, moved from `zebra-consensus` so that contextual validation can use them ([#11454](https://github.com/ZcashFoundation/zebra/pull/11454)).
- `Block::transaction_fees` and `parameters::subsidy::{nsm_value_balance_is_tracked, nsm_fee_contribution}` support NU7 fee accounting. From NU7 activation, the NSM receives 60% of aggregate block transaction fees, rounded down, and `miner_fees_are_valid` excludes that contribution from the miner payout ([#11487](https://github.com/ZcashFoundation/zebra/pull/11487), [#11530](https://github.com/ZcashFoundation/zebra/pull/11530)).
- `parameters::subsidy::cumulative_scheduled_issuance` sums the scheduled subsidy across slow start, spacing upgrades and halvings, including the scheduled genesis subsidy ([#11529](https://github.com/ZcashFoundation/zebra/pull/11529)).
- `parameters::Network::directory_name`: a persistent storage namespace that includes configured Testnet wire magic. ([#11527](https://github.com/ZcashFoundation/zebra/pull/11527))
- `NetworkUpgrade::duration_between_heights`, the target block time between two heights with each block charged the target spacing at its own height ([#11483](https://github.com/ZcashFoundation/zebra/issues/11483)).

### Changed

- Use the [ZIP 259](https://zips.z.cash/zip-0259) NU7 consensus branch ID `0x77190AD9` for v5 and v6 transactions instead of placeholder branch IDs. ([#11527](https://github.com/ZcashFoundation/zebra/pull/11527))
- `parameters::subsidy::height_for_halving` applies the slow-start shift once before Blossom and NU7 spacing changes, including when the first halving precedes Blossom ([#11529](https://github.com/ZcashFoundation/zebra/pull/11529)).
- `parameters::subsidy::subsidy_is_valid` requires fixed lockbox disbursements even when the total block subsidy is zero, without requiring zero-valued proportional funding outputs ([#11530](https://github.com/ZcashFoundation/zebra/pull/11530)).
- `parameters::testnet::ParametersBuilder::is_compatible_with_default_parameters` includes the temporary Orchard-disabling height in public Testnet compatibility checks ([#11527](https://github.com/ZcashFoundation/zebra/pull/11527)).
- Chain-tip height estimates use the candidate block spacing across upgrade boundaries in both directions and floor negative fractional time offsets ([#11529](https://github.com/ZcashFoundation/zebra/pull/11529)).
- `parameters::testnet::ParametersBuilder::with_activation_heights` rejects out-of-order explicit upgrades before coalescing equal heights, preventing a later upgrade from hiding an invalid earlier activation ([#11527](https://github.com/ZcashFoundation/zebra/pull/11527)).
- Converting an activation map into `parameters::testnet::ConfiguredActivationHeights` preserves implicit upgrades at shared heights. This prevents `Parameters::new_regtest` from assigning earlier default heights when a configured Regtest is serialized and loaded again ([#11527](https://github.com/ZcashFoundation/zebra/pull/11527)).
- `parameters::testnet::ParametersBuilder::with_network_name` rejects reserved network names case-insensitively. ([#11527](https://github.com/ZcashFoundation/zebra/pull/11527))
- [ZIP 2008](https://zips.z.cash/zip-2008) rotates the Mainnet FPF recipient from `t3cFfPt1Bcvgez9ZbMBFWeZsskxTkPzGCow` to the P2PKH address `t1MkHnkxVjNpNbCrSs3AJ8J7ZSp6NTYiUcG` at the first address period after the period containing the last pre-NU7 block. Funding-output validation accepts the assigned P2PKH recipient. Mainnet NU7 activation remains unassigned. ([#11527](https://github.com/ZcashFoundation/zebra/pull/11527))
- `parameters::constants::activation_heights::testnet::NU7` exposes the assigned height 4,465,026, and `parameters::Network::new_default_testnet` activates NU7 there. The adjusted third halving is at 4,497,948, and [ZIP 237](https://zips.z.cash/zip-0237) derives the first NSM reissuance height as 7,305,222. Mainnet NU7 activation remains unassigned ([#11554](https://github.com/ZcashFoundation/zebra/pull/11554)).

## [13.0.1] - 2026-09-25

### Security

- Reject malformed V6 transactions during deserialization to prevent a remotely triggerable denial of service ([GHSA-h5rr-8pqv-grp9](https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-h5rr-8pqv-grp9)).

## [13.0.0] - 2026-09-23

### Breaking Changes

- `transaction::Transaction` is now a newtype wrapping `zcash_primitives::transaction::Transaction`, rather than an enum of Zebra-owned structs. Its variants and their fields are no longer public: use the accessor methods, and the `Transaction::test_v*` constructors under the `proptest-impl` feature to build transactions in tests ([#10461](https://github.com/ZcashFoundation/zebra/pull/10461)).
- Transaction and block accessors that returned references to shielded data now return owned values, because that data is owned by `zcash_primitives`. Callers no longer need `.cloned()` ([#10461](https://github.com/ZcashFoundation/zebra/pull/10461)).
- Transaction serialization is delegated to `zcash_primitives`. The `ZcashSerialize` and `ZcashDeserialize` impls for Zebra's own `sapling`, `orchard` and `ironwood` shielded-data types are now compiled only for tests, so they are no longer part of the released API ([#10461](https://github.com/ZcashFoundation/zebra/pull/10461)).
- Removed `transaction::txid::TxIdBuilder` and the `transaction::builder` module. Transaction IDs and coinbase construction now come from `zcash_primitives` ([#10461](https://github.com/ZcashFoundation/zebra/pull/10461)).

### Added

- `transaction::Transaction::ironwood_anchor`, `has_enough_ironwood_flags`, `orchard_proof_size_is_canonical` and `ironwood_proof_size_is_canonical` ([#10461](https://github.com/ZcashFoundation/zebra/pull/10461)).
- `transaction::sprout_joinsplit_key_proof_and_ciphertexts`, which reads the `ephemeralKey`, zero-knowledge proof, and `encCiphertexts` of a Sprout JoinSplit. `zcash_primitives` does not expose accessors for those fields, and only exposes the proof for the Groth16 (V4) variant, not PHGR13 (V2/V3) ([#10461](https://github.com/ZcashFoundation/zebra/pull/10461), [#11415](https://github.com/ZcashFoundation/zebra/pull/11415)).
- With the `proptest-impl` feature: `transaction::arbitrary::shielded`, which builds Orchard and Ironwood bundles for tests, and `Transaction::test_v6`, `test_v5_with_orchard` and `test_v6_with_bundles` ([#10461](https://github.com/ZcashFoundation/zebra/pull/10461)).

### Changed

- `transaction::zip317::MARGINAL_FEE` is now 1000 zatoshis per logical action instead of 5000, per [zcash/zips#1352](https://github.com/zcash/zips/pull/1352); `conventional_fee()` and `unpaid_actions()` follow it ([#11290](https://github.com/ZcashFoundation/zebra/pull/11290)).

## [12.0.0] - 2026-08-10

### Breaking Changes

- Added the `ValueBalanceError::Total` variant returned when the sum of value
  pools is out of range
  ([#10817](https://github.com/ZcashFoundation/zebra/pull/10817)).

### Added

- `ValueBalance::total`, which returns the sum of all value pool balances
  ([#10817](https://github.com/ZcashFoundation/zebra/pull/10817)).

## [11.3.0] - 2026-07-27

### Added

- `ironwood::ShieldedData::data_mut`
- With the `proptest-impl` feature:
  - `transaction::Transaction::v6_strategy`
  - `impl Arbitrary for orchard::ShieldedDataV6` and `ironwood::ShieldedData`
  - `parameters::NetworkUpgrade::nu6_3_branch_id_strategy`
  - `transaction::Transaction::ironwood_value_balance_mut`

### Changed

- `AT_OR_NEAR_TIP_THRESHOLD` is now 1,000 blocks, keeping peer stall detection disabled during
  long gaps between blocks ([#11122](https://github.com/ZcashFoundation/zebra/pull/11122)).
- Updated `zcash_primitives` and `zcash_proofs` to 0.30, `zcash_keys` to 0.16, and
  `zcash_transparent` to 0.10
  ([#11111](https://github.com/ZcashFoundation/zebra/pull/11111)).

### Fixed

- With the `proptest-impl` feature, the arbitrary `Transaction` strategy now generates v6
  transactions (with v6 Orchard and Ironwood bundles) for NU6.3 and later network upgrades,
  and accepts a transaction version override of 6. Previously it only generated v4/v5
  transactions for those upgrades, so property tests built on it could never observe
  v6/Ironwood data ([#11075](https://github.com/ZcashFoundation/zebra/pull/11075)).

## [11.2.0] - 2026-07-17

### Added

- `block::Header::{SERIALIZED_SIZE, REGTEST_SERIALIZED_SIZE, serialized_size}`
- `transaction::zip317::MARGINAL_FEE`
- `work::equihash::Solution::{SERIALIZED_SIZE, REGTEST_SERIALIZED_SIZE, serialized_size}`

### Security

- Computing `transaction::Transaction::value_balance` no longer clones the entire UTXO map per
  call (GHSA-4g24-549m-hp75).

## [11.1.0] - 2026-07-10

### Added

- NU6.3 (Ironwood) now activates on Mainnet at height 3,428,143, matching `zcash_protocol`
- `AT_OR_NEAR_TIP_THRESHOLD` constant and `ChainTip::is_at_or_near_network_tip()`
  method for determining whether the node is within 5 blocks of the estimated network tip
  ([#10732](https://github.com/ZcashFoundation/zebra/pull/10732))

### Changed

- MSRV is now 1.88
- Updated `zcash_protocol`, `zcash_primitives`, `zcash_keys`, `zcash_address`,
  `zcash_transparent`, `zcash_proofs`, `zcash_history`, and `orchard` to their released
  NU6.3 versions

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

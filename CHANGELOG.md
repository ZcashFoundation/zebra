# CHANGELOG

All notable changes to Zebra are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org).

## Unreleased

### Breaking Changes

- Migrate `zebrad` to a layered configuration using config-rs. Environment variables must use the
  `ZEBRA_SECTION__KEY` format (double underscore for nesting), for example:
  `ZEBRA_NETWORK__NETWORK`, `ZEBRA_RPC__LISTEN_ADDR`, `ZEBRA_RPC__ENABLE_COOKIE_AUTH`,
  `ZEBRA_RPC__COOKIE_DIR`, `ZEBRA_TRACING__FILTER`, `ZEBRA_STATE__CACHE_DIR`,
  `ZEBRA_MINING__MINER_ADDRESS`. Legacy `ZEBRA_*` test/path variables and `ZEBRA_RUST_LOG` are no
  longer honored. Update any scripts, Docker configs, or systemd units that relied on the old names
  ([#9768](https://github.com/ZcashFoundation/zebra/pull/9768)).

- Docker entrypoint simplified: it no longer generates a `zebrad.toml` or translates legacy Docker
  environment variables. To use a file, set `CONFIG_FILE_PATH` (the entrypoint forwards it via
  `--config`). Otherwise, configure via `ZEBRA_*` variables. `ZEBRA_CONF_PATH` has been removed in
  favor of `CONFIG_FILE_PATH`. Docker setups that used variables like `ZEBRA_RPC_PORT`,
  `ZEBRA_COOKIE_DIR`, `NETWORK`, `ENABLE_COOKIE_AUTH`, or `MINER_ADDRESS` must switch to the
  config-rs equivalents shown above ([#9768](https://github.com/ZcashFoundation/zebra/pull/9768)).

- Fully removed the `getblocktemplate-rpcs` feature flag from `zebrad/Cargo.toml`.
  All functionality previously guarded by this flag has already been made the default.
  As a result, the following build command is no longer supported:
  ```
  cargo build --features getblocktemplate-rpcs
  ```
  ([#9964](https://github.com/ZcashFoundation/zebra/pull/9964))
- Expects the block commitment bytes of Heartwood activation blocks to be the `hashBlockCommitments` after NU5 activation

### Changed

- `zebrad` now loads configuration from defaults, an optional TOML file, and environment variables,
  with precedence: Env > TOML > Defaults ([#9768](https://github.com/ZcashFoundation/zebra/pull/9768)).
- Docker and book documentation updated to describe `CONFIG_FILE_PATH` and `ZEBRA_*` environment
  variable usage; removed references to `ZEBRA_CONF_PATH` and legacy Docker variables
  ([#9768](https://github.com/ZcashFoundation/zebra/pull/9768)).
- Added deserialization logic to call `extend_funding_streams()` when the flag is true for both configured Testnets and Regtest

## [Zebra 2.5.0](https://github.com/ZcashFoundation/zebra/releases/tag/v2.5.0) - 2025-08-07

This release includes the implementation of Zcash **Network Upgrade 6.1** (NU6.1) on Testnet and sets its activation height on the public Testnet at block **3,536,500**. Please update your Testnet nodes as soon as possible to ensure compatibility.

### Breaking Changes

- Value pool "deferred" changes its identifier to "lockbox". `getblock` and `getblockchaininfo` RPC methods will now return `lockbox` as the `FS_DEFERRED` value pool to match [zcashd](https://github.com/zcash/zcash/pull/6912/files#diff-decae4be02fb8a47ab4557fe74a9cb853bdfa3ec0fa1b515c0a1e5de91f4ad0bR276). ([#9684](https://github.com/ZcashFoundation/zebra/pull/9684))

### Added

- Implement one-time lockbox disbursement mechanism for NU6.1 ([#9603](https://github.com/ZcashFoundation/zebra/pull/9603), [#9757](https://github.com/ZcashFoundation/zebra/pull/9757), [#9747](https://github.com/ZcashFoundation/zebra/pull/9747), [#9754](https://github.com/ZcashFoundation/zebra/pull/9754))
- NU6.1 Testnet implementation and deployment ([#9762](https://github.com/ZcashFoundation/zebra/pull/9762), [#9759](https://github.com/ZcashFoundation/zebra/pull/9759))

### Fixed

- `validateaddress` RPC bug ([#9734](https://github.com/ZcashFoundation/zebra/pull/9734))
- Count sigops using generics ([#9670](https://github.com/ZcashFoundation/zebra/pull/9670))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@Galoretka, @arya2, @conradoplg, @dorianvp, @gustavovalverde, @oxarbitrage, @pacu and @upbqdn


## [Zebra 2.4.2](https://github.com/ZcashFoundation/zebra/releases/tag/v2.4.2) - 2025-07-28

This release fixes a database upgrade bug that was introduced in the 2.4.0
release (which has been removed). If you have upgraded to 2.4.0, your Zebra
address index has become corrupted. This does not affect consensus, but will
make the RPC interface return invalid data for calls like `getaddressutxos` and
other address-related calls.

**(Also refer to the 2.4.0 release notes below for important breaking changes.)**

Zebra 2.4.2 prints a warning upon starting if you have been impacted by the bug.
The log line will look like:

```
2025-07-17T17:12:41.636549Z  WARN zebra_state::service::finalized_state::zebra_db: You have been impacted by the Zebra 2.4.0 address indexer corruption bug. If you rely on the data from the RPC interface, you will need to recover your database. Follow the instructions in the 2.4.2 release notes: https://github.com/ZcashFoundation/zebra/releases/tag/v2.4.2 If you just run the node for consensus and don't use data from the RPC interface, you can ignore this warning.
```

If you rely on the RPC data, you will need to restore your database. If you have
backed up the state up before upgrading to 2.4.0, you can simply restore the backup
and run 2.4.2 from it. If you have not, you have two options:

- Stop Zebra, delete the state (e.g. `~/.cache/zebra/state/v27/mainnet` and
  `testnet` too if applicable), upgrade to 2.4.2 (if you haven't already), and
  start Zebra. It will sync from scratch, which will take around 48 hours to
  complete, depending on the machine specifications.
- Use the `copy-state` subcommand to regenerate a valid state.
  This will require an additional ~300 GB of free disk size. It is likely that
  it will take around the same time as syncing from scratch, but it has the
  advantage of not depending on the network.
  - Stop Zebra.
  - Rename the old corrupted state folder, e.g. `mv ~/.cache/zebra ~/.cache/zebra.old`
  - Copy your current `zebrad.toml` file to a `zebrad-source.toml` file and edit
    the `cache_dir` config to the renamed folder, e.g. `cache_dir = '/home/zebrad/.cache/zebra.old'`
  - The `copy-state` command that will be run requires a bigger amount of opened
    files. Increase the limit by running `ulimit -n 2048`; refer to your OS
    documentation if that does not work.
  - Run the `copy-state` command: `zebrad -c zebrad-source.toml copy-state
    --target-config-path zebrad.toml`. The command will take several hours to
    complete.

### Fixed

- Fix 2.4.0 DB upgrade; add warning if impacted ([#9709](https://github.com/ZcashFoundation/zebra/pull/9709))
- Downgrade verbose mempool message ([#9700](https://github.com/ZcashFoundation/zebra/pull/9700))

### Contributors

Thanks to @ebfull for reporting the bug and helping investigating its cause.


## Zebra 2.4.1 - \[REMOVED\]

This version of Zebra wasn't fully published; it was tagged but the tag was
removed, and it was published on `crates.io` but it was yanked. It was not
published on Docker Hub.

We removed it due to a panic that happened during the pre-release validation.
However, we determined that the panic was caused by an external tool (`ldb
checkpoint`) being used internally to make database backups and it was not a bug
in Zebra.


## [Zebra 2.4.0](https://github.com/ZcashFoundation/zebra/releases/tag/v2.4.0) - 2025-07-11 \[REMOVED\]

### Breaking Changes

This release has the following breaking changes:

- This release contains a major database upgrade. It will upgrade an existing
  database, automatically moving it from the `v26/` folder to a new `v27/`
  folder. However, downgrading is not possible. If you want to keep the
  possibility of reverting Zebra in case of an unexpected issue, backup the
  `v26/` folder _before_ running the upgraded Zebra. Note that the upgrade is slow
  and could take several hours or more to complete on smaller machines. Zebra will
  operate normally during that time; the only difference is that some RPC
  responses might return empty or not accurate data (pool values for arbitrary
  block heights and received balances for addresses).
- While this was never documented as an option for backups, if you relied on the
  `ldb checkpoint` tool to generate database backups, be advised that the tool
  is no longer supported and _will_ corrupt databases generated or touched by
  Zebra 2.4.0 or later releases.
- The `debug_like_zcashd` config option for mining is no longer available. It
  was not enabled by default; if it is now present in the config file, Zebra
  will panic. Simply delete the config option to fix.
- The `cm_u` field byte order was fixed in `getrawtransaction`/`getblock`
  response, so if you relied on the wrong order, you will need to fix your
  application.
- The `zebra-scan` and `zebra-grpc` crates are no longer supported and were
  removed from the codebase.


### Security

- Fix a consensus rule violation in V5 coinbase transactions at low heights. This issue could only occur on Regtest or custom testnets and is now resolved  ([#9620](https://github.com/ZcashFoundation/zebra/pull/9620))

### Added

- Implemented deserialization for Zebra's block and transaction types ([#9522](https://github.com/ZcashFoundation/zebra/pull/9522))
- Update `getaddressbalance` RPC to return `received` field ([#9295](https://github.com/ZcashFoundation/zebra/pull/9295), [#9539](https://github.com/ZcashFoundation/zebra/pull/9539))
- Added a `mempool_change()` gRPC method for listening to changes in the mempool ([#9494](https://github.com/ZcashFoundation/zebra/pull/9494))
- Added `raw_value` feature to serde\_json ([#9538](https://github.com/ZcashFoundation/zebra/pull/9538))
- Modified `zebra_network::Config` type to use IPv6 listen\_addr by default ([#9609](https://github.com/ZcashFoundation/zebra/pull/9609))
- Added `invalidateblock` and `reconsiderblock` RPC methods ([#9551](https://github.com/ZcashFoundation/zebra/pull/9551))
- Updated `(z_)validateaddress` to validate TEX addresses ([#9483](https://github.com/ZcashFoundation/zebra/pull/9483))
- Added a `addnode` RPC method ([#9604](https://github.com/ZcashFoundation/zebra/pull/9604))
- Added missing fields to getrawtransaction ([#9636](https://github.com/ZcashFoundation/zebra/pull/9636))
- Added value pool balances to `getblock` RPC output ([#9432](https://github.com/ZcashFoundation/zebra/pull/9432), [#9539](https://github.com/ZcashFoundation/zebra/pull/9539))
- Added support for configuring shielded addresses for mining ([#9574](https://github.com/ZcashFoundation/zebra/pull/9574))
- Added binding_sig, joinsplit_pub_key and joinsplit_sig fields to `getrawtransaction`/`getblock` response ([#9652](https://github.com/ZcashFoundation/zebra/pull/9652))
- Added a method in `zebra-rpc` to allow validating addresses ([#9658](https://github.com/ZcashFoundation/zebra/pull/9658))

### Changed

- Allow Zebra crates to be compiled with alternative versions of their dependencies ([#9484](https://github.com/ZcashFoundation/zebra/pull/9484))
- Updated README with Arch build patch ([#9513](https://github.com/ZcashFoundation/zebra/pull/9513))
- Renamed and moved exports in `zebra-rpc` ([#9568](https://github.com/ZcashFoundation/zebra/pull/9568))
- Upgraded DB format to support new fields in RPC outputs ([#9539](https://github.com/ZcashFoundation/zebra/pull/9539))
- Moved GBT RPCs into the main RPC server ([#9459](https://github.com/ZcashFoundation/zebra/pull/9459))
- Added a `Nu6_1` variant to `NetworkUpgrade` ([#9526](https://github.com/ZcashFoundation/zebra/pull/9526))
- Use zcash\_script’s new `Script` trait ([#8751](https://github.com/ZcashFoundation/zebra/pull/8751))
- Removed `debug_like_zcashd` config option ([#9627](https://github.com/ZcashFoundation/zebra/pull/9627))
- Sync all chains in `TrustedChainSync::sync`, add `NonFinalizedStateChange` gRPC method ([#9654](https://github.com/ZcashFoundation/zebra/pull/9654))
- Added `prometheus` as a default feature in zebrad ([#9677](https://github.com/ZcashFoundation/zebra/pull/9677))

### Fixed

- Preserve order of RPC output fields ([#9474](https://github.com/ZcashFoundation/zebra/pull/9474))
- Fixed `cm_u` field byte order in `getrawtransaction`/`getblock` response ([#9667](https://github.com/ZcashFoundation/zebra/pull/9667))
- Avoid repeatedly converting transactions to librustzcash types when computing sighashes ([#9594](https://github.com/ZcashFoundation/zebra/pull/9594))
- Correctly set optional `scriptPubKey` fields of transactions in `getblock` and `getrawtransaction` RPC outputs ([#9536](https://github.com/ZcashFoundation/zebra/pull/9536))
- Allow local outbound connections on Regtest ([#9580](https://github.com/ZcashFoundation/zebra/pull/9580))
- Allow for parsing `z_gettreestate` output type where optional fields are omitted ([#9451](https://github.com/ZcashFoundation/zebra/pull/9451))

### Removed

- Removed `zebra-scan` and `zebra-grpc` ([#9683](https://github.com/ZcashFoundation/zebra/pull/9683))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@ala-mode, @arya2, @conradoplg, @elijahhampton, @gustavovalverde, @idky137, @mpguerra, @oxarbitrage, @sellout, @str4d and @upbqdn


## [Zebra 2.3.0](https://github.com/ZcashFoundation/zebra/releases/tag/v2.3.0) - 2025-05-06

### Breaking Changes

- The RPC endpoint is no longer enabled by default in Docker. To enable it,
  follow the docs [here](https://zebra.zfnd.org/user/docker.html#rpc).
- We will no longer be publishing Docker images tagged with the `sha-` or `v`
  prefixes. If you use tags starting with the `v` prefix, please update to
  images tagged `N.N.N`. For example, use `2.3.0` instead of `v2.3.0`. If you
  need a specific hash, each tag has a digest that you can use instead.
- All functionality that used to be guarded by the `getblocktemplate-rpcs` Cargo
  feature was moved out and the feature is no longer present in the codebase.
  Note that all release builds following Zebra 1.3.0 had this feature enabled by
  default.

### Added

- Track misbehaving peer connections and ban them past a threshold ([#9201](https://github.com/ZcashFoundation/zebra/pull/9201))
- Restore internal miner ([#9311](https://github.com/ZcashFoundation/zebra/pull/9311))
- Add `reconsider_block` method to non-finalized state ([#9260](https://github.com/ZcashFoundation/zebra/pull/9260))
- Add NU7 constants ([#9256](https://github.com/ZcashFoundation/zebra/pull/9256))
- Add `invalidate_block_method` and `invalidated_blocks` field to non-finalized state ([#9167](https://github.com/ZcashFoundation/zebra/pull/9167))
- Add unused `Transaction::V6` variant ([#9339](https://github.com/ZcashFoundation/zebra/pull/9339))

### Changed

- Downgrade verbose info message ([#9448](https://github.com/ZcashFoundation/zebra/pull/9448))
- Use read-only db instance when running `tip-height` or `copy-state` commands ([#9359](https://github.com/ZcashFoundation/zebra/pull/9359))
- Refactor format upgrades into trait ([#9263](https://github.com/ZcashFoundation/zebra/pull/9263))
- Remove the `getblocktemplate-rpcs` Cargo feature ([#9401](https://github.com/ZcashFoundation/zebra/pull/9401))
- Improve cache dir and database startup panics ([#9441](https://github.com/ZcashFoundation/zebra/pull/9441))
- Added `txid` field to `TransactionObject` ([#9617](https://github.com/ZcashFoundation/zebra/issues/9617))
### Fixed

- Remove a redundant startup warning ([#9397](https://github.com/ZcashFoundation/zebra/pull/9397))
- Advertise mined blocks ([#9176](https://github.com/ZcashFoundation/zebra/pull/9176))
- Ensure secondary rocksdb instance has caught up to the primary instance ([#9346](https://github.com/ZcashFoundation/zebra/pull/9346))
- Use network kind of `TestnetKind` in transparent addresses on Regtest ([#9175](https://github.com/ZcashFoundation/zebra/pull/9175))
- Fix redundant attributes on enum variants ([#9309](https://github.com/ZcashFoundation/zebra/pull/9309))

### RPCs

- Add `time` and `size` fields to `TransactionObject` ([#9458](https://github.com/ZcashFoundation/zebra/pull/9458))
- Add inbound peers to `getpeerinfo` response ([#9214](https://github.com/ZcashFoundation/zebra/pull/9214))
- Extend `getinfo` ([#9261](https://github.com/ZcashFoundation/zebra/pull/9261))
- Add fields to `getblockchaininfo` RPC output ([#9215](https://github.com/ZcashFoundation/zebra/pull/9215))
- Add some missing fields to transaction object ([#9329](https://github.com/ZcashFoundation/zebra/pull/9329))
- Support negative heights in `HashOrHeight` ([#9316](https://github.com/ZcashFoundation/zebra/pull/9316))
- Add verbose support to getrawmempool ([#9249](https://github.com/ZcashFoundation/zebra/pull/9249))
- Fill size field in getblock with verbosity=2 ([#9327](https://github.com/ZcashFoundation/zebra/pull/9327))
- Add `blockcommitments` field to `getblock` output ([#9217](https://github.com/ZcashFoundation/zebra/pull/9217))
- Accept an unused second param in `sendrawtransaction` RPC ([#9242](https://github.com/ZcashFoundation/zebra/pull/9242))
- Make start and end fields optional and apply range rules to match zcashd ([#9408](https://github.com/ZcashFoundation/zebra/pull/9408))
- Return only the history tree root in `GetBlockTemplateChainInfo` response ([#9444](https://github.com/ZcashFoundation/zebra/pull/9444))
- Correctly map JSON-RPC to/from 2.0 ([#9216](https://github.com/ZcashFoundation/zebra/pull/9216))
- Permit JSON-RPC IDs to be non-strings ([#9341](https://github.com/ZcashFoundation/zebra/pull/9341))
- Match coinbase outputs order in `Getblocktemplate` ([#9272](https://github.com/ZcashFoundation/zebra/pull/9272))

### Docker

- Refactor Dockerfile and entrypoint ([#8923](https://github.com/ZcashFoundation/zebra/pull/8923))
- Enhance Zebra configuration options and entrypoint logic ([#9344](https://github.com/ZcashFoundation/zebra/pull/9344))
- Better permission and cache dirs handling in Docker ([#9323](https://github.com/ZcashFoundation/zebra/pull/9323))
- Allow r/w access in mounted volumes ([#9281](https://github.com/ZcashFoundation/zebra/pull/9281))

### Documentation

- Update examples for running Zebra in Docker ([#9269](https://github.com/ZcashFoundation/zebra/pull/9269))
- Add architectural decision records structure ([#9310](https://github.com/ZcashFoundation/zebra/pull/9310))
- Add Mempool Specification to Zebra Book ([#9336](https://github.com/ZcashFoundation/zebra/pull/9336))
- Complete the Treestate RFC documentation ([#9340](https://github.com/ZcashFoundation/zebra/pull/9340))

### Contributors

@AloeareV, @Metalcape, @PaulLaux, @VolodymyrBg, @aphelionz, @arya2, @conradoplg,
@crStiv, @elijahhampton, @gustavovalverde, @mdqst, @natalieesk, @nuttycom,
@oxarbitrage, @podZzzzz, @sellout, @str4d, @upbqdn and @zeroprooff.

## [Zebra 2.2.0](https://github.com/ZcashFoundation/zebra/releases/tag/v2.2.0) - 2025-02-03

In this release, Zebra introduced an additional consensus check on the branch ID of Nu6 transactions
(which is currently also checked elsewhere; but we believe it's important to check on its own to protect
against future code changes), along with important refactors and improvements.

### Added

- An index to track spending transaction ids by spent outpoints and revealed nullifiers ([#8895](https://github.com/ZcashFoundation/zebra/pull/8895))
- Accessor methods to `zebra-rpc` request/response types ([#9113](https://github.com/ZcashFoundation/zebra/pull/9113))
- `getblock` RPC method now can return transaction details with verbosity=2 ([#9083](https://github.com/ZcashFoundation/zebra/pull/9083))
- Serialized NU5 blocks to test vectors ([#9098](https://github.com/ZcashFoundation/zebra/pull/9098))

### Changed

- Migrated from deprecated `jsonrpc_*` crates to `jsonrpsee` ([#9059](https://github.com/ZcashFoundation/zebra/pull/9059), [#9151](https://github.com/ZcashFoundation/zebra/pull/9151))
- Optimized checks for coinbase transactions ([#9126](https://github.com/ZcashFoundation/zebra/pull/9126))
- Avoid re-verifying transactions in blocks if those transactions are in the mempool ([#8951](https://github.com/ZcashFoundation/zebra/pull/8951), [#9118](https://github.com/ZcashFoundation/zebra/pull/9118))
- Allow transactions spending coinbase outputs to have transparent outputs on Regtest ([#9085](https://github.com/ZcashFoundation/zebra/pull/9085))

### Fixed

- Respond to getblockchaininfo with genesis block when empty state ([#9138](https://github.com/ZcashFoundation/zebra/pull/9138))
- Verify consensus branch ID in SIGHASH precomputation ([#9139](https://github.com/ZcashFoundation/zebra/pull/9139))
- More closely match zcashd RPC errors and `getrawtransaction` RPC behaviour ([#9049](https://github.com/ZcashFoundation/zebra/pull/9049))
- Fixes bugs in the lightwalletd integration tests ([#9052](https://github.com/ZcashFoundation/zebra/pull/9052))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@Fallengirl, @arya2, @conradoplg, @elijahhampton, @futreall, @gustavovalverde, @idky137, @mpguerra, @oxarbitrage, @rex4539, @rootdiae, @sandakersmann and @upbqdn


## [Zebra 2.1.0](https://github.com/ZcashFoundation/zebra/releases/tag/v2.1.0) - 2024-12-06

This release adds a check to verify that V5 transactions in the mempool have the correct consensus branch ID;
Zebra would previously accept those and return a transaction ID (indicating success) even though they would
be eventually rejected by the block consensus checks. Similarly, Zebra also now returns an error when trying
to submit transactions that would eventually fail some consensus checks (e.g. double spends) but would also
return a transaction ID indicating success.  The release also bumps
Zebra's initial minimum protocol version such that this release of Zebra will always reject connections with peers advertising
a network protocol version below 170,120 on Mainnet and 170,110 on Testnet instead of accepting those connections until Zebra's
chain state reaches the NU6 activation height.
The `getblock` RPC method has been updated and now returns some additional information
such as the block height (even if you provide a block hash) and other fields as supported
by the `getblockheader` RPC call.

### Breaking Changes

- Upgrade minimum protocol versions for all Zcash networks ([#9058](https://github.com/ZcashFoundation/zebra/pull/9058))

### Added

- `getblockheader` RPC method ([#8967](https://github.com/ZcashFoundation/zebra/pull/8967))
- `rust-toolchain.toml` file ([#8985](https://github.com/ZcashFoundation/zebra/pull/8985))

### Changed

- Updated `getblock` RPC to more closely match zcashd ([#9006](https://github.com/ZcashFoundation/zebra/pull/9006))
- Updated error messages to include inner error types (notably for the transaction verifier) ([#9066](https://github.com/ZcashFoundation/zebra/pull/9066))

### Fixed

- Validate consensus branch ids of mempool transactions ([#9063](https://github.com/ZcashFoundation/zebra/pull/9063))
- Verify mempool transactions with unmined inputs if those inputs are in the mempool to support TEX transactions ([#8857](https://github.com/ZcashFoundation/zebra/pull/8857))
- Wait until transactions have been added to the mempool before returning success response from `sendrawtransaction` RPC ([#9067](https://github.com/ZcashFoundation/zebra/pull/9067))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @conradoplg, @cypherpepe, @gustavovalverde, @idky137, @oxarbitrage, @pinglanlu and @upbqdn

## [Zebra 2.0.1](https://github.com/ZcashFoundation/zebra/releases/tag/v2.0.1) - 2024-10-30

- Zebra now supports NU6 on Mainnet. This patch release updates dependencies
  required for NU6. The 2.0.0 release was pointing to the incorrect dependencies
  and would panic on NU6 activation.

### Breaking Changes

- The JSON RPC endpoint has cookie-based authentication enabled by default.
  **If you rely on Zebra RPC, you will need to adjust your config.** The
  simplest change is to disable authentication by adding `enable_cookie_auth =
  false` to the `[rpc]` section of the Zebra config file; [refer to the
  docs for more information](https://zebra.zfnd.org/user/lightwalletd.html#json-rpc) (this was added
  in v2.0.0, but is being mentioned again here for clarity).

### Changed

- Use ECC deps with activation height for NU6
  ([#8960](https://github.com/ZcashFoundation/zebra/pull/8978))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @gustavovalverde, @oxarbitrage and @upbqdn.

## [Zebra 2.0.0](https://github.com/ZcashFoundation/zebra/releases/tag/v2.0.0) - 2024-10-25 - \[REMOVED\]

This release was intended to support NU6 but was pointing to the wrong version
of dependencies which would make Zebra panic at NU6 activation. Use v2.0.1 instead.

### Breaking Changes

- Zebra now supports NU6 on Mainnet.
- The JSON RPC endpoint has a cookie-based authentication enabled by default.
  **If you rely on Zebra RPC, you will need to adjust your config.** The
  simplest change is to disable authentication by adding `enable_cookie_auth =
  false` to the `[rpc]` section of the Zebra config file; [refer to the
  docs](https://zebra.zfnd.org/user/lightwalletd.html#json-rpc).

### Added

- NU6-related documentation ([#8949](https://github.com/ZcashFoundation/zebra/pull/8949))
- A cookie-based authentication system for the JSON RPC endpoint ([#8900](https://github.com/ZcashFoundation/zebra/pull/8900), [#8965](https://github.com/ZcashFoundation/zebra/pull/8965))

### Changed

-  Set the activation height of NU6 for Mainnet and bumped Zebra's current network protocol version ([#8960](https://github.com/ZcashFoundation/zebra/pull/8960))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @gustavovalverde, @oxarbitrage and @upbqdn.

## [Zebra 2.0.0-rc.0](https://github.com/ZcashFoundation/zebra/releases/tag/v2.0.0-rc.0) - 2024-10-11

This version is a release candidate for the Zcash NU6 network upgrade on the Mainnet. While this version does not yet include the NU6 Mainnet activation height or current protocol version, all required functionality and tests are in place.

Please note that support for this release candidate is expected to conclude prior to the NU6 activation heights.

### Security

- Added Docker Scout vulnerabilities scanning ([#8871](https://github.com/ZcashFoundation/zebra/pull/8871))

### Added

- Added Regtest-only `generate` and `stop` RPC methods ([#8849](https://github.com/ZcashFoundation/zebra/pull/8849), [#8839](https://github.com/ZcashFoundation/zebra/pull/8839), [#8863](https://github.com/ZcashFoundation/zebra/pull/8863))
- Added fields to `getmininginfo` RPC method response ([#8860](https://github.com/ZcashFoundation/zebra/pull/8860))
- Copied the Python RPC test framework from zcashd into Zebra ([#8866](https://github.com/ZcashFoundation/zebra/pull/8866))

### Changed

- Regtest halving interval to match zcashd and added a configurable halving interval for custom testnets ([#8888](https://github.com/ZcashFoundation/zebra/pull/8888), [#8928](https://github.com/ZcashFoundation/zebra/pull/8928))
- Updates post-NU6 Major Grants funding stream address on Mainnet ([#8914](https://github.com/ZcashFoundation/zebra/pull/8914))

### Fixed

- Remove debugging output by default in Docker image ([#8870](https://github.com/ZcashFoundation/zebra/pull/8870))
- Fixes a typo in configuration file path of the docker-compose file ([#8893](https://github.com/ZcashFoundation/zebra/pull/8893))
- Return verification errors from `sendrawtransaction` RPC method ([#8788](https://github.com/ZcashFoundation/zebra/pull/8788))
- Respond to getheaders requests with a maximum of 160 block headers ([#8913](https://github.com/ZcashFoundation/zebra/pull/8913))
- Avoids panicking during contextual validation when a parent block is missing ([#8883](https://github.com/ZcashFoundation/zebra/pull/8883))
- Write database format version to disk atomically to avoid a rare panic ([#8795](https://github.com/ZcashFoundation/zebra/pull/8795))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @dismad, @gustavovalverde, @oxarbitrage, @skyl and @upbqdn


## [Zebra 1.9.0](https://github.com/ZcashFoundation/zebra/releases/tag/v1.9.0) - 2024-08-02

This release includes deployment of NU6 on Testnet, configurable funding streams on custom Testnets, and updates Zebra's end-of-support (EoS)
from 16 weeks to 10 weeks so that it will panic before the expected activation height of NU6 on Mainnet.

It also replaces the `shielded-scan` compilation feature with a new `zebra-scanner` binary, adds a `TrustedChainSync` module
for replicating Zebra's best chain state, and a gRPC server in `zebra-rpc` as steps towards zcashd deprecation.

#### Recovering after finalizing a block from a chain fork

Zebra doesn't enforce an end-of-support height on Testnet, and previous versions of Zebra could mine or follow a chain fork that does not
activate NU6 on Testnet at height 2976000. Once a block from a fork is finalized in Zebra's state cache, updating to a version of Zebra that
does expect NU6 activation at that height will result in Zebra getting stuck, as it cannot rollback its finalized state. This can be resolved
by syncing Zebra from scratch, or by using the `copy-state` command to create a new state cache up to height 2975999. To use the `copy-state`
command, first make a copy Zebra's Testnet configuration with a different cache directory path, for example, if Zebra's configuration is at the
default path, by running `cp ~/.config/zebrad.toml ./zebrad-copy-target.toml`, then opening the new configuration file and editing the
`cache_dir` path in the `state` section. Once there's a copy of Zebra's configuration with the new state cache directory path, run:
`zebrad copy-state --target-config-path "./zebrad-copy-target.toml" --max-source-height "2975999"`, and then update the original
Zebra configuration to use the new state cache directory.

### Added

- A `zebra-scanner` binary replacing the `shielded-scan` compilation feature in `zebrad` ([#8608](https://github.com/ZcashFoundation/zebra/pull/8608))
- Adds a `TrustedChainSync` module for keeping up with Zebra's non-finalized best chain from a separate process ([#8596](https://github.com/ZcashFoundation/zebra/pull/8596))
- Add a tonic server in zebra-rpc with a `chain_tip_change()` method that notifies clients when Zebra's best chain tip changes ([#8674](https://github.com/ZcashFoundation/zebra/pull/8674))
- NU6 network upgrade variant, minimum protocol version, and Testnet activation height ([#8693](https://github.com/ZcashFoundation/zebra/pull/8693), [8733](https://github.com/ZcashFoundation/zebra/pull/8733), [#8804](https://github.com/ZcashFoundation/zebra/pull/8804))
- Configurable NU6 activation height on Regtest ([#8700](https://github.com/ZcashFoundation/zebra/pull/8700))
- Configurable Testnet funding streams ([#8718](https://github.com/ZcashFoundation/zebra/pull/8718))
- Post-NU6 funding streams, including a lockbox funding stream ([#8694](https://github.com/ZcashFoundation/zebra/pull/8694))
- Add value pool balances to `getblockchaininfo` RPC method response ([#8769](https://github.com/ZcashFoundation/zebra/pull/8769))
- Add NU6 lockbox funding stream information to `getblocksubsidy` RPC method response ([#8742](https://github.com/ZcashFoundation/zebra/pull/8742))

### Changed

- Reduce the end-of-support halt time from 16 weeks to 10 weeks ([#8734](https://github.com/ZcashFoundation/zebra/pull/8734))
- Bump the current protocol version from 170100 to 170110 ([#8804](https://github.com/ZcashFoundation/zebra/pull/8804))
- Track the balance of the deferred chain value pool ([#8732](https://github.com/ZcashFoundation/zebra/pull/8732), [#8729](https://github.com/ZcashFoundation/zebra/pull/8729))
- Require that coinbase transactions balance exactly after NU6 activation ([#8727](https://github.com/ZcashFoundation/zebra/pull/8727))
- Support in-place disk format upgrades for major database version bumps ([#8748](https://github.com/ZcashFoundation/zebra/pull/8748))

### Fixed

- Return full network upgrade activation list in `getblockchaininfo` method ([#8699](https://github.com/ZcashFoundation/zebra/pull/8699))
- Update documentation for the new `zebra-scanner` binary ([#8675](https://github.com/ZcashFoundation/zebra/pull/8675))
- Update documentation for using Zebra with lightwalletd ([#8714](https://github.com/ZcashFoundation/zebra/pull/8714))
- Reduce debug output for Network on Regtest and default Testnet ([#8760](https://github.com/ZcashFoundation/zebra/pull/8760))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @conradoplg, @dependabot[bot], @oxarbitrage, @therealyingtong and @upbqdn


## [Zebra 1.8.0](https://github.com/ZcashFoundation/zebra/releases/tag/v1.8.0) - 2024-07-02

- Zebra now uses a default unpaid actions limit of 0, dropping transactions with
  any unpaid actions from the mempool and excluding them when selecting
  transactions from the mempool during block template construction.
- The `zebrad` binary no longer contains the scanner of shielded transactions.
  This means `zebrad` no longer contains users' viewing keys.
- Support for custom Testnets and Regtest is greatly enhanced.
- Windows is now back in the second tier of supported platforms.
- The end-of-support time interval is set to match `zcashd`'s 16 weeks.
- The RPC serialization of empty treestates matches `zcashd`.

### Added

- Add an init function for a standalone `ReadStateService` ([#8595](https://github.com/ZcashFoundation/zebra/pull/8595))
- Allow configuring more parameters on custom Testnets and an NU5 activation height on Regtest ([#8477](https://github.com/ZcashFoundation/zebra/pull/8477), [#8518](https://github.com/ZcashFoundation/zebra/pull/8518), [#8524](https://github.com/ZcashFoundation/zebra/pull/8524), [#8528](https://github.com/ZcashFoundation/zebra/pull/8528), [#8636](https://github.com/ZcashFoundation/zebra/pull/8636))
- Add default constructions for several RPC method responses ([#8616](https://github.com/ZcashFoundation/zebra/pull/8616), [#8505](https://github.com/ZcashFoundation/zebra/pull/8505))
- Support constructing Canopy proposal blocks from block templates ([#8505](https://github.com/ZcashFoundation/zebra/pull/8505))

#### Docs

- Document custom Testnets, Regtest and how they compare to Mainnet and the default Testnet in the Zebra book ([#8526](https://github.com/ZcashFoundation/zebra/pull/8526), [#8636](https://github.com/ZcashFoundation/zebra/pull/8636))

### Changed

- Use a default unpaid action limit of 0 so that transactions with unpaid actions are excluded from the mempool and block templates by default ([#8638](https://github.com/ZcashFoundation/zebra/pull/8638))
- Lower the mandatory checkpoint height from immediately above the ZIP-212 grace period to immediately below Canopy activation ([#8629](https://github.com/ZcashFoundation/zebra/pull/8629))
- Use the new `zcash_script` callback API in `zebra-script`, allowing Zebra's ECC dependencies to be upgraded independently of `zcash_script` and `zcashd` ([#8566](https://github.com/ZcashFoundation/zebra/pull/8566))
- Put Windows in Tier 2 of supported platforms ([#8637](https://github.com/ZcashFoundation/zebra/pull/8637))
- Remove experimental support for starting the `zebra-scan` in the `zebrad` process as a step towards moving the scanner to its own process ([#8594](https://github.com/ZcashFoundation/zebra/pull/8594))
- Reduce the end of support time from 20 weeks to 16 weeks ([#8530](https://github.com/ZcashFoundation/zebra/pull/8530))
- Restore parts of the experimental `internal-miner` feature for use on Regtest ([#8506](https://github.com/ZcashFoundation/zebra/pull/8506))

### Fixed

- Allow square brackets in network config listen addr and external addr without explicitly configuring a port in the listen addr ([#8504](https://github.com/ZcashFoundation/zebra/pull/8504))
- Fix general conditional compilation attributes ([#8602](https://github.com/ZcashFoundation/zebra/pull/8602))
- Update `median_timespan()` method to align with zcashd implementation ([#8491](https://github.com/ZcashFoundation/zebra/pull/8491))
- Allow custom Testnets to make peer connections ([#8528](https://github.com/ZcashFoundation/zebra/pull/8528))
- Refactor and simplify the serialization of empty treestates to fix an incorrect serialization of empty note commitment trees for RPCs ([#8533](https://github.com/ZcashFoundation/zebra/pull/8533))
- Fix port conflict issue in some tests and re-enable those tests on Windows where those issues were especially problematic ([#8624](https://github.com/ZcashFoundation/zebra/pull/8624))
- Fix a bug with trailing characters in the OpenAPI spec method descriptions ([#8597](https://github.com/ZcashFoundation/zebra/pull/8597))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @conradoplg, @gustavovalverde, @oxarbitrage and @upbqdn

## [Zebra 1.7.0](https://github.com/ZcashFoundation/zebra/releases/tag/v1.7.0) - 2024-05-07

In this release we introduce Regtest functionality to Zebra and restored Windows support. Also adjusted our Zebra release interval from 2 weeks to 6 weeks approximately.

### Added

- Preparing for upstream `zcash_client_backend` API changes ([#8425](https://github.com/ZcashFoundation/zebra/pull/8425))
- Regtest support ([#8383](https://github.com/ZcashFoundation/zebra/pull/8383), [#8421](https://github.com/ZcashFoundation/zebra/pull/8421), [#8368](https://github.com/ZcashFoundation/zebra/pull/8368), [#8413](https://github.com/ZcashFoundation/zebra/pull/8413), [#8474](https://github.com/ZcashFoundation/zebra/pull/8474), [#8475](https://github.com/ZcashFoundation/zebra/pull/8475))
- Allow Zebra users to contribute to the P2P network even if behind NAT or firewall ([#8488](https://github.com/ZcashFoundation/zebra/pull/8488))

### Changed

- Adjust estimated release interval to once every 6 weeks and the end of support from 16 to 20 weeks ([#8429](https://github.com/ZcashFoundation/zebra/pull/8429))

### Fixed

- Bump zcash script v0.1.15 and restore Windows support ([#8393](https://github.com/ZcashFoundation/zebra/pull/8393))
- Avoid possibly returning data from different blocks in `z_get_treestate` RPC method ([#8460](https://github.com/ZcashFoundation/zebra/pull/8460))
- Zebra panics with all features and no elasticsearch server available ([#8409](https://github.com/ZcashFoundation/zebra/pull/8409))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @oxarbitrage and @upbqdn


## [Zebra 1.6.1](https://github.com/ZcashFoundation/zebra/releases/tag/v1.6.1) - 2024-04-15

This release adds an OpenAPI specification for Zebra's RPC methods and startup logs about Zebra's storage usage and other database information.

It also includes:
- Bug fixes and improved error messages for some zebra-scan gRPC methods
- A performance improvement in Zebra's `getblock` RPC method

### Added

- Log database information such as storage usage on startup and shutdown ([#8336](https://github.com/ZcashFoundation/zebra/pull/8336), [#8389](https://github.com/ZcashFoundation/zebra/pull/8389))
- OpenAPI specification for Zebra's RPC methods ([#8342](https://github.com/ZcashFoundation/zebra/pull/8342))
- Add block times to output of getblock RPC method when called with `verbosity = 2` ([#8384](https://github.com/ZcashFoundation/zebra/pull/8384))

### Changed

- Removed `Copy` trait impl for `Network` ([#8354](https://github.com/ZcashFoundation/zebra/pull/8354))
- Refactored code for network consensus parameters to `Network` methods ([#8340](https://github.com/ZcashFoundation/zebra/pull/8340))

### Fixed

- Improve zebra-scan gRPC method errors and add timeout to scan service to avoid hanging ([#8318](https://github.com/ZcashFoundation/zebra/pull/8318))
- Await state service requests in `getblock` method in parallel ([#8376](https://github.com/ZcashFoundation/zebra/pull/8376))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @elijahhampton, @gustavovalverde, @idky137, @mpguerra, @oxarbitrage, @upbqdn and @zancas

## [Zebra 1.6.0](https://github.com/ZcashFoundation/zebra/releases/tag/v1.6.0) - 2024-02-23

This release exposes the shielded scanning functionality through an initial
version of a gRPC server, documented in the [Zebra
Book](https://zebra.zfnd.org/user/shielded-scan-grpc-server.html).

> [!NOTE]
> Building Zebra now depends on
> [`protoc`](https://github.com/protocolbuffers/protobuf). See the [Build
> Instructions](https://github.com/ZcashFoundation/zebra?tab=readme-ov-file#building-zebra)
> for more details.

### Added

- Add `docker-compose` file to run CI locally ([#8209](https://github.com/ZcashFoundation/zebra/pull/8209))
- Allow users to use Zebra + LWD with persistent states ([#8215](https://github.com/ZcashFoundation/zebra/pull/8215))

#### Scanner

- Add a new `zebra-grpc` crate ([#8167](https://github.com/ZcashFoundation/zebra/pull/8167))
- Start scanner gRPC server with `zebrad` ([#8241](https://github.com/ZcashFoundation/zebra/pull/8241))
- Add gRPC server reflection and document how to use the gRPC server ([#8288](https://github.com/ZcashFoundation/zebra/pull/8288))
- Add the `GetInfo` gRPC method ([#8178](https://github.com/ZcashFoundation/zebra/pull/8178))
- Add the `GetResults` gRPC method ([#8255](https://github.com/ZcashFoundation/zebra/pull/8255))
- Add the `Scan` gRPC method ([#8268](https://github.com/ZcashFoundation/zebra/pull/8268), [#8303](https://github.com/ZcashFoundation/zebra/pull/8303))
- Add the `RegisterKeys` gRPC method ([#8266](https://github.com/ZcashFoundation/zebra/pull/8266))
- Add the `ClearResults` and `DeleteKeys` gRPC methods ([#8237](https://github.com/ZcashFoundation/zebra/pull/8237))
- Add snapshot tests for new gRPCs ([#8277](https://github.com/ZcashFoundation/zebra/pull/8277))
- Add unit tests for new gRPCs ([#8293](https://github.com/ZcashFoundation/zebra/pull/8293))
- Create a tower Service in `zebra-scan` ([#8185](https://github.com/ZcashFoundation/zebra/pull/8185))
- Implement the `SubscribeResults` scan service request ([#8253](https://github.com/ZcashFoundation/zebra/pull/8253))
- Implement the `ClearResults` scan service request ([#8219](https://github.com/ZcashFoundation/zebra/pull/8219))
- Implement the `DeleteKeys` scan service request ([#8217](https://github.com/ZcashFoundation/zebra/pull/8217))
- Implement the `RegisterKeys` scan service request ([#8251](https://github.com/ZcashFoundation/zebra/pull/8251))
- Implement the `Results` scan service request ([#8224](https://github.com/ZcashFoundation/zebra/pull/8224))
- Test the `RegisterKeys` scan service request ([#8281](https://github.com/ZcashFoundation/zebra/pull/8281))
- Add `ViewingKey` type in `zebra-chain` ([#8198](https://github.com/ZcashFoundation/zebra/pull/8198))
- Handle `RegisterKeys` messages in scan task ([#8222](https://github.com/ZcashFoundation/zebra/pull/8222))

### Changed

- Remove `rfc.md` file ([#8228](https://github.com/ZcashFoundation/zebra/pull/8228))
- Update Debian from Bullseye to Bookworm in Docker ([#8273](https://github.com/ZcashFoundation/zebra/pull/8273))
- Remove Zebra RFCs from `CONTRIBUTING.md` ([#8304](https://github.com/ZcashFoundation/zebra/pull/8304))
- Publish fewer tags in Docker Hub ([#8300](https://github.com/ZcashFoundation/zebra/pull/8300))
- Add Zebra crate versions to dev-dependencies and remove circular dev-dependencies ([#8171](https://github.com/ZcashFoundation/zebra/pull/8171))
- Update docs for building Zebra ([#8315](https://github.com/ZcashFoundation/zebra/pull/8315))

### Fixed

- Set log rotation to avoid docker bugs ([#8269](https://github.com/ZcashFoundation/zebra/pull/8269))
- Improve error message in `non_blocking_logger` test ([#8276](https://github.com/ZcashFoundation/zebra/pull/8276))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @bishopcheckmate, @chairulakmal, @gustavovalverde, @mpguerra, @oxarbitrage and @upbqdn.

## [Zebra 1.5.2](https://github.com/ZcashFoundation/zebra/releases/tag/v1.5.2) - 2024-01-23

This release serves as a hotfix for version 1.5.1, addressing issues encountered after its initial release. For more information about version 1.5.1, refer to [this link](https://github.com/ZcashFoundation/zebra/releases/tag/v1.5.2).

Following the release on GitHub, we identified difficulties in publishing the `zebra-chain` crate to crates.io. Detailed information is available in [issue #8180](https://github.com/ZcashFoundation/zebra/issues/8180) and its references.

Unfortunately, to resolve this challenge, which involves an unpublished dependency, we had to temporarily remove the internal miner support introduced in version 1.5.1.

In our efforts to reinstate this feature, we've opened a tracking ticket to monitor the progress of the required code that must be merged into the `equihash` dependency. You can follow the developments in [issue #8183](https://github.com/ZcashFoundation/zebra/issues/8183), which will only be closed once the feature is successfully restored.

### Breaking Changes

- Temporally remove the internal miner functionality ([#8184](https://github.com/ZcashFoundation/zebra/pull/8184))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@oxarbitrage


## [Zebra 1.5.1](https://github.com/ZcashFoundation/zebra/releases/tag/v1.5.1) - 2024-01-18

This release:

- Adds a utility for reading scanning results, and finalizes the MVP features of the scanner.
- Adds an experimental `internal-miner` feature, which mines blocks within `zebrad`. This feature is only supported on testnet. Use a more efficient GPU or ASIC for mainnet mining.
- Contains many documentation improvements.

### Added

- Add an internal Zcash miner to Zebra ([#8136](https://github.com/ZcashFoundation/zebra/pull/8136), [#8150](https://github.com/ZcashFoundation/zebra/pull/8150))
- Blockchain scanner new features:
  - Don't scan and log if we are below sapling height ([#8121](https://github.com/ZcashFoundation/zebra/pull/8121))
  - Restart scanning where left ([#8080](https://github.com/ZcashFoundation/zebra/pull/8080))
  - Add scanning result reader utility ([#8104](https://github.com/ZcashFoundation/zebra/pull/8104), [#8157](https://github.com/ZcashFoundation/zebra/pull/8157))
- Note default path to config in docs ([#8143](https://github.com/ZcashFoundation/zebra/pull/8143))
- Document how to add a column family ([#8149](https://github.com/ZcashFoundation/zebra/pull/8149))

### Changed

- Make sure scanner database is accessed using the correct types ([#8112](https://github.com/ZcashFoundation/zebra/pull/8112))
- Move history tree and value balance to typed column families ([#8115](https://github.com/ZcashFoundation/zebra/pull/8115))
- Refactor the user documentation for scanning  ([#8124](https://github.com/ZcashFoundation/zebra/pull/8124))
- Refactor user \& dev documentation ([#8145](https://github.com/ZcashFoundation/zebra/pull/8145))
- Improve feature flag docs ([#8114](https://github.com/ZcashFoundation/zebra/pull/8114))
- Allow opening the database in a read-only mode ([#8079](https://github.com/ZcashFoundation/zebra/pull/8079))
- Send all zebrad logs to the journal under systemd ([#7965](https://github.com/ZcashFoundation/zebra/pull/7965))

### Fixed

- Point to a manually created list of Zebra crates in docs ([#8160](https://github.com/ZcashFoundation/zebra/pull/8160))
- Add shielded-scan.md to the index ([#8095](https://github.com/ZcashFoundation/zebra/pull/8095))
- Elasticsearch feature, make bulk size the same for testnet and mainnet ([#8127](https://github.com/ZcashFoundation/zebra/pull/8127))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @bishopcheckmate, @gustavovalverde, @oxarbitrage, @sandakersmann, @teor2345 and @upbqdn


## [Zebra 1.5.0](https://github.com/ZcashFoundation/zebra/releases/tag/v1.5.0) - 2023-11-28

This release:
- fixes a panic that was introduced in Zebra v1.4.0, which happens in rare circumstances when reading cached sprout or history trees.
- further improves how Zebra recovers from network interruptions and prevents potential network hangs.
- limits the ability of synthetic nodes to spread throughout the network through Zebra to address some of the Ziggurat red team report.

As of this release, Zebra requires Rust 1.73 to build.

Finally, we've added an experimental "shielded-scan" feature and the zebra-scan crate as steps
towards supporting shielded scanning in Zebra. This feature has known security issues.
It is for experimental use only. Ongoing development is tracked in issue [#7728](https://github.com/ZcashFoundation/zebra/issues/7728).

### Important Security Warning

Do not use regular or sensitive viewing keys with Zebra's experimental scanning feature. Do not use this
feature on a shared machine. We suggest generating new keys for experimental use.

### Security

- security(net): Stop sending peer addresses from version messages directly to the address book ([#7977](https://github.com/ZcashFoundation/zebra/pull/7977))
- security(net): Limit how many addresses are sent directly to the address book for a single peer address message ([#7952](https://github.com/ZcashFoundation/zebra/pull/7952))
- security(net): Rate-limit GetAddr responses to avoid sharing the entire address book over a short period ([#7955](https://github.com/ZcashFoundation/zebra/pull/7955))

### Added

- feat(config): Add config field for the viewing keys used by zebra-scan ([#7949](https://github.com/ZcashFoundation/zebra/pull/7949))
- feat(scan): Add on-disk database to store keys and scan results ([#7942](https://github.com/ZcashFoundation/zebra/pull/7942), [#8036](https://github.com/ZcashFoundation/zebra/pull/8036))
- feat(scan): Spawn zebra-scan task from zebrad with configured viewing keys ([#7989](https://github.com/ZcashFoundation/zebra/pull/7989))
- feat(scan): Create a scan_block function to use across scanning tasks ([#7994](https://github.com/ZcashFoundation/zebra/pull/7994))
- feat(scan): Scan blocks with Sapling keys and write the results to the database ([#8040](https://github.com/ZcashFoundation/zebra/pull/8040))
- poc(scan): Proof of concept for shielded scanning ([#7758](https://github.com/ZcashFoundation/zebra/pull/7758))
- add(docker): Add `ldb` RocksDB query tool to the Dockerfile ([#8074](https://github.com/ZcashFoundation/zebra/pull/8074))

### Changed

- change(state): Expose ZebraDb methods that can create different kinds of databases ([#8002](https://github.com/ZcashFoundation/zebra/pull/8002))
- change(state): Make the types for finalized blocks consistent ([#7923](https://github.com/ZcashFoundation/zebra/pull/7923))
- change(scan): Create a scanner storage database ([#8031](https://github.com/ZcashFoundation/zebra/pull/8031))
- change(scan): Store scanned TXIDs in "display order" ([#8057](https://github.com/ZcashFoundation/zebra/pull/8057))
- change(scan): Create a function that scans one block by height, and stores the results in the database ([#8045](https://github.com/ZcashFoundation/zebra/pull/8045))
- change(scan): Store one transaction ID per database row, to make queries easier ([#8062](https://github.com/ZcashFoundation/zebra/pull/8062))
- change(log): Silence verbose failed connection logs ([#8072](https://github.com/ZcashFoundation/zebra/pull/8072))

### Fixed

- fix(db): Fix a sprout/history tree read panic in Zebra v1.4.0, which only happens before the 25.3.0 state upgrade completes ([#7972](https://github.com/ZcashFoundation/zebra/pull/7972))
- fix(net): Fix potential network hangs, and reduce code complexity ([#7859](https://github.com/ZcashFoundation/zebra/pull/7859))
- fix(scan): Start scanning task only if there are keys to scan ([#8059](https://github.com/ZcashFoundation/zebra/pull/8059))
- fix(rpc): Make the `verbose` argument of the `getrawtransaction` RPC optional ([#8076](https://github.com/ZcashFoundation/zebra/pull/8076))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @oxarbitrage, @teor2345 and @upbqdn

## [Zebra 1.4.0](https://github.com/ZcashFoundation/zebra/releases/tag/v1.4.0) - 2023-11-07

Zebra's mining RPCs are now available in release builds. Our Docker images are significantly
smaller, because the smaller Zcash verification parameters are now built into the `zebrad` binary.
Zebra has updated to the shared Rust dependencies from the `zcashd` 5.7.0 release.

Zebra recovers better from brief network interruptions, and avoids some network and verification
denial of service and performance issues. We have restored our macOS tests in CI, and now support
macOS on a best-effort basis.

We have changed our documentation website URL, and we are considering deprecating some Docker image
tags in release 1.5.0 and later.

### Deprecation Warnings

This release has the following deprecation warnings:

#### Warning: Deprecation of DockerHub Image Tags in a future release

Zebra currently publishes 11 [DockerHub tags](https://hub.docker.com/r/zfnd/zebra/tags) for each new release.
We want to reduce the number of DockerHub tags we publish in a future minor Zebra release.

Based on usage and user feedback, we could stop publishing:
- The `1` tag, which updates each release until NU6
- The `1.x` tag, which updates each patch release until the next minor release
- The `1.x.y` tag, which is the same as `v1.x.y`
- The `sha-xxxxxxx` tag, which is the same as `v1.x.y` (for production releases)

We also want to standardise experimental image tags to `-experimental`, rather than `.experimental`.

So for release 1.5.0, we might only publish these tags:
- `latest`
- `latest-experimental` (a new tag)
- `v1.5.0`
- `v1.5.0-experimental`

Please let us know if you need any other tags by [opening a GitHub ticket](https://github.com/ZcashFoundation/zebra/issues/new?assignees=&labels=C-enhancement%2CS-needs-triage&projects=&template=feature_request.yml&title=feature%3A+).

We recommend using the `latest` tag to always get the most recent Zebra release.

#### Warning: Documentation Website URL Change

We have replaced the API documentation on the [doc.zebra.zfnd.org](https://doc.zebra.zfnd.org)
website with [docs.rs](https://docs.rs/releases/search?query=zebra). All links have been updated.

Zebra's API documentation can be found on:
- [`docs.rs`](https://docs.rs/releases/search?query=zebra), which renders documentation for the
  public API of the latest crate releases;
- [`doc-internal.zebra.zfnd.org`](https://doc-internal.zebra.zfnd.org/), which renders
  documentation for the internal API on the `main` branch.

[doc.zebra.zfnd.org](https://doc.zebra.zfnd.org) stopped being updated a few days before this release,
and it will soon be shut down.

### Significant Changes

This release contains the following significant changes:

#### Mining RPCs in Production Builds

Zebra's mining RPCs are now available in release builds (#7740). Any Zebra instance can be used
by a solo miner or mining pool. This stabilises 12 RPCs, including  `getblocktemplate`, `submitblock`,
`getmininginfo`, `getnetworksolps`, `[z_]validateaddress` and `getblocksubsidy`. For more information,
read our [mining blog post](https://zfnd.org/experimental-mining-support-in-zebra/).

Please [let us know](https://github.com/ZcashFoundation/zebra/issues/new?assignees=&labels=C-enhancement%2CS-needs-triage&projects=&template=feature_request.yml&title=feature%3A+)
if your mining pool needs extra RPC methods or fields.

#### Zcash Parameters in `zebrad` Binary

`zebrad` now bundles zk-SNARK parameters directly into its binary. This increases the binary size
by a few megabytes, but reduces the size of the Docker image by around 600 MB because
the parameters don't contain the Sprout proving key anymore. The `zebrad download`
command does nothing, so it has been removed.

Previously, parameters were stored by default in these locations:

* `~/.zcash-params` (on Linux); or
* `~/Library/Application Support/ZcashParams` (on Mac); or
* `C:\Users\Username\AppData\Roaming\ZcashParams` (on Windows)

If you have upgraded `zebrad` to 1.4.0 or later, and `zcashd` to 5.7.0 or later, you can delete the
parameter files in these directories to save approximately 700 MB disk space.

[`zcashd` have deprecated their `fetch-params.sh` script](https://github.com/zcash/zcash/blob/master/doc/release-notes/release-notes-5.7.0.md#deprecation-of-fetch-paramssh),
so it can't be used to retry failed downloads in `zebrad` 1.3.0 and earlier.

We recommend upgrading to the latest Zebra release to avoid download issues in new installs.

#### macOS Support

macOS x86_64 is now supported on a best-effort basis. macOS builds and some tests run in Zebra's CI.

### Security

- Reconnect with peers after brief network interruption ([#7853](https://github.com/ZcashFoundation/zebra/pull/7853))
- Add outer timeouts for critical network operations to avoid hangs ([#7869](https://github.com/ZcashFoundation/zebra/pull/7869))
- Set iterator read bounds where possible in DiskDb, to avoid a known RocksDB denial of service issue ([#7731](https://github.com/ZcashFoundation/zebra/pull/7731), [#7732](https://github.com/ZcashFoundation/zebra/pull/7732))
- Fix concurrency issues in tree key formats, and CPU usage in genesis tree roots ([#7392](https://github.com/ZcashFoundation/zebra/pull/7392))

### Removed

- Remove the `zebrad download` command, because it no longer does anything ([#7819](https://github.com/ZcashFoundation/zebra/pull/7819))

### Added

- Enable mining RPCs by default in production builds ([#7740](https://github.com/ZcashFoundation/zebra/pull/7740))
- Re-enable macOS builds and tests in CI ([#7834](https://github.com/ZcashFoundation/zebra/pull/7834))
- Make macOS x86_64 a tier 2 supported platform in the docs ([#7843](https://github.com/ZcashFoundation/zebra/pull/7843))
- Add macOS M1 as a tier 3 supported platform ([#7851](https://github.com/ZcashFoundation/zebra/pull/7851))

### Changed

- Build Sprout and Sapling parameters into the zebrad binary, so a download server isn't needed ([#7800](https://github.com/ZcashFoundation/zebra/pull/7800), [#7844](https://github.com/ZcashFoundation/zebra/pull/7844))
- Bump ECC dependencies for `zcashd` 5.7.0 ([#7784](https://github.com/ZcashFoundation/zebra/pull/7784))
- Refactor the installation instructions for the `s-nomp` mining pool software ([#7835](https://github.com/ZcashFoundation/zebra/pull/7835))

### Fixed

- Make the `latest` Docker tag point to the production build, rather than the build with experimental features ([#7817](https://github.com/ZcashFoundation/zebra/pull/7817))
- Fix an incorrect consensus-critical ZIP 212 comment ([#7774](https://github.com/ZcashFoundation/zebra/pull/7774))
- Fix broken links to `zebra_network` and `zebra_state` `Config` structs on doc-internal.zebra.zfnd.org ([#7838](https://github.com/ZcashFoundation/zebra/pull/7838))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @gustavovalverde, @mpguerra, @oxarbitrage, @rex4539, @teor2345, @upbqdn, and @vuittont60.

## [Zebra 1.3.0](https://github.com/ZcashFoundation/zebra/releases/tag/v1.3.0) - 2023-10-16

This release adds RPC methods for the "Spend before Sync" light wallet feature,
and fixes performance issues and bugs in the mining solution rate RPCs. Progress
bars can now be enabled using a config, please help us test them!

It contains the following updates:

### User Testing: Progress Bars

Zebra has progress bars! When progress bars are enabled, you can see Zebra's blocks,
transactions, and peer connections in your terminal. We're asking Zebra users to test this
feature, and give us [feedback on the forums](https://forum.zcashcommunity.com/t/zebra-progress-bars/44485).

To show progress bars while running Zebra, add these lines to your `zebrad.toml`:
```toml
[tracing]
progress_bar = "summary"
```

For more details, including a known issue with time estimates,
read our [progress bars blog post](https://zfnd.org/experimental-zebra-progress-bars/).

### Security

- Fix database concurrency bugs that could have led to panics or incorrect history tree data (#7590, #7663)

### Added

- Zebra's progress bars can now be enabled using a `zebrad.toml` config (#7615)
- Add missing elasticsearch flag feature to lib docs (#7568)
- Add missing Docker variables and examples (#7552)
- Check database format is valid on startup and shutdown (#7566, #7606). We expect to catch almost all database validity errors in CI (#7602, #7627), so users are unlikely to see them on startup or shutdown.

#### Spend before Sync Support

- Add state requests and support code for the `z_getsubtreesbyindex` RPC (#7408, #7734)
- Implement the `z_getsubtreesbyindex` RPC (#7436)
- Test the `z_getsubtreesbyindex` RPC (#7515, #7521, #7566, #7514, #7628)
- Format subtree roots in little-endian order (#7466)
- Add note subtree indexes for new and existing blocks (#7437)
- Upgrade subtrees from the tip backwards, for compatibility with wallet syncing (#7531)
- Handle a subtree comparison edge case correctly (#7587)

### Changed

- Return errors instead of panicking in methods for Heights (#7591)
- Update tests for compatibility with the ECC's `lightwalletd` fork (#7349)

### Fixed

- Refactor docs for feature flags (#7567)
- Match zcashd's getblockchaininfo capitalisation for NU5 (#7454)
- Fix bugs and performance of `getnetworksolps` & `getnetworkhashps` RPCs (#7647)

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @gustavovalverde, @oxarbitrage, @rex4539, @teor2345 and @upbqdn.

## [Zebra 1.2.0](https://github.com/ZcashFoundation/zebra/releases/tag/v1.2.0) - 2023-09-01

### Highlights

This release:

- Starts our work implementing "spend before sync" algorithm for lightwalletd.
- Contains an automatic database upgrade that reduces the size of Zebra's current cached state from approximately 276GB to 244GB. It does so by automatically pruning unneeded note commitment trees from an existing cache. New Zebra instances will also build their cache without these trees.

### Breaking Changes

`zebrad` 1.2.0 cached states are incompatible with previous `zebrad` versions:

- `zebrad` 1.2.0 upgrades the cached state format. The new format is incompatible with previous `zebrad` versions. After upgrading to this Zebra version, don't downgrade to an earlier version.
- When earlier versions try to use states upgraded by `zebrad` 1.2.0:
    - `zebrad` versions 1.0.0 and 1.0.1 will respond to some `z_gettreestate` RPC requests with incorrect empty `final_state` fields
    - pre-release `zebrad` versions can panic when verifying shielded transactions, updating the state, or responding to RPC requests

### Changed

- Deduplicate note commitment trees stored in the finalized state ([#7312](https://github.com/ZcashFoundation/zebra/pull/7312), [#7379](https://github.com/ZcashFoundation/zebra/pull/7379))
- Insert only the first tree in each series of identical trees into finalized state ([#7266](https://github.com/ZcashFoundation/zebra/pull/7266))
- Our testing framework now uses the ECC lightwalletd fork ([#7307](https://github.com/ZcashFoundation/zebra/pull/7307)). This was needed to start the work of implementing fast spendability. The ECC repo is now the supported implementation in Zebra, documentation was changed to reflect this. ([#7427](https://github.com/ZcashFoundation/zebra/pull/7427))

### Added

- Documentation for mining with Docker ([#7179](https://github.com/ZcashFoundation/zebra/pull/7179))
- Note tree sizes field to `getblock` RPC method ([#7278](https://github.com/ZcashFoundation/zebra/pull/7278))
- Note commitment subtree types to zebra-chain ([#7371](https://github.com/ZcashFoundation/zebra/pull/7371))
- Note subtree index handling to zebra-state, but we're not writing subtrees to the finalized state yet ([#7334](https://github.com/ZcashFoundation/zebra/pull/7334))

### Fixed

- Log a warning instead of panicking for unused mining configs ([#7290](https://github.com/ZcashFoundation/zebra/pull/7290))
- Avoid expensive note commitment tree root recalculations in eq() methods ([#7386](https://github.com/ZcashFoundation/zebra/pull/7386))
- Use the correct state version for databases without a state version file ([#7385](https://github.com/ZcashFoundation/zebra/pull/7385))
- Avoid temporary failures verifying the first non-finalized block or attempting to fork the chain before the final checkpoint ([#6810](https://github.com/ZcashFoundation/zebra/pull/6810))
- If a database format change is cancelled, also cancel the format check, and don't mark the database as upgraded ([#7442](https://github.com/ZcashFoundation/zebra/pull/7442))

## [Zebra 1.1.0](https://github.com/ZcashFoundation/zebra/releases/tag/v1.1.0) - 2023-07-18

This release adds new mempool metrics, fixes panics when cancelling tasks on shutdown, detects subcommand name typos on the command-line, and improves the usability of Zebra's Docker images (particularly for mining).

### Breaking Changes

- Zebra now detects subcommand name typos on the command-line. If you want to give Zebra a list of tracing filters, use `zebrad start --filters debug,...` ([#7056](https://github.com/ZcashFoundation/zebra/pull/7056))

### Security

- Avoid initiating outbound handshakes with IPs for which Zebra already has an active peer ([#7029](https://github.com/ZcashFoundation/zebra/pull/7029))
- Rate-limit inbound connections per IP ([#7041](https://github.com/ZcashFoundation/zebra/pull/7041))

### Added

- Metrics tracking mempool actions and size bucketed by weight ([#7019](https://github.com/ZcashFoundation/zebra/pull/7019)) by @str4d
- Legacy state format compatibility layer and version bumps for ECC dependencies to match `zcashd` 5.6.0 ([#7053](https://github.com/ZcashFoundation/zebra/pull/7053))
- Framework for upcoming in-place database format upgrades ([#7031](https://github.com/ZcashFoundation/zebra/pull/7031))


### Changed

- Deduplicate note commitment trees in non-finalized state ([#7218](https://github.com/ZcashFoundation/zebra/pull/7218), [#7239](https://github.com/ZcashFoundation/zebra/pull/7239))

### Fixed

- Enable miners running Zebra with Docker to set their address for mining rewards ([#7178](https://github.com/ZcashFoundation/zebra/pull/7178))
- Use default RPC port when running Zebra with Docker ([#7177](https://github.com/ZcashFoundation/zebra/pull/7177), [#7162](https://github.com/ZcashFoundation/zebra/pull/7162))
- Stop panicking on async task cancellation on shutdown in network and state futures ([#7219](https://github.com/ZcashFoundation/zebra/pull/7219))
- Remove redundant startup logs, fix progress bar number, order, and wording ([#7087](https://github.com/ZcashFoundation/zebra/pull/7087))
- Organize Docker `ENV` and `ARG` values based on their usage ([#7200](https://github.com/ZcashFoundation/zebra/pull/7200))
- Avoid blocking threads by awaiting proof verification results from rayon in async context ([#6887](https://github.com/ZcashFoundation/zebra/pull/6887))


### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @gustavovalverde, @mpguerra, @oxarbitrage, @str4d, @teor2345 and @upbqdn


## [Zebra 1.0.1](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.1) - 2023-07-03

Zebra's first patch release fixes multiple peer connection security issues and panics. It also significantly reduces Zebra's CPU usage. We recommend that all users upgrade to Zebra 1.0.1 or later.

As of this release, Zebra requires Rust 1.70 to build. macOS builds are no longer officially supported by the Zebra team.

If you're running `zebrad` in a terminal, you'll see a new Zebra welcome message.

Please report bugs to [the Zebra GitHub repository](https://github.com/ZcashFoundation/zebra/issues/new?assignees=&labels=C-bug%2C+S-needs-triage&projects=&template=bug_report.yml&title=)

### Breaking Changes

This release has the following breaking changes:
- Zebra limits each IP address to 1 peer connection, to prevent denial of service attacks. This can be changed using the `network.max_connections_per_ip` config. ([#6980](https://github.com/ZcashFoundation/zebra/pull/6980), [#6993](https://github.com/ZcashFoundation/zebra/pull/6993), [#7013](https://github.com/ZcashFoundation/zebra/pull/7013)).
  Thank you to @dimxy from komodo for reporting this bug, and the Ziggurat team for demonstrating
  its impact on testnet.
- Zebra uses new APIs in Rust 1.70 to prevent concurrency bugs that could cause hangs or panics
  ([#7032](https://github.com/ZcashFoundation/zebra/pull/7032)).

### Support Changes

These platforms are no longer supported by the Zebra team:
- macOS has been moved from tier 2 to [tier 3 support](https://github.com/ZcashFoundation/zebra/blob/main/book/src/user/supported-platforms.md#tier-3) ([#6965](https://github.com/ZcashFoundation/zebra/pull/6965)). We disabled our regular macOS builds because Rust 1.70 [causes crashes during shutdown on macOS x86_64 (#6812)](https://github.com/ZcashFoundation/zebra/issues/6812). Zebra's state uses database transactions, so it should not be corrupted by the crash.

### Security

- Use Arc::into\_inner() to avoid potential hangs or panics ([#7032](https://github.com/ZcashFoundation/zebra/pull/7032))
- Replace openssl with rustls in tests and experimental features ([#7047](https://github.com/ZcashFoundation/zebra/pull/7047))

#### Network Security

- Fix long delays in accepting inbound handshakes, and delays in async operations throughout Zebra. ([#7103](https://github.com/ZcashFoundation/zebra/pull/7103)). Thank you to the Ziggurat Team for reporting this bug.
- Limit each IP address to 1 peer connection, to prevent denial of service attacks. ([#6980](https://github.com/ZcashFoundation/zebra/pull/6980), [#6993](https://github.com/ZcashFoundation/zebra/pull/6993))
- Close new peer connections from the same IP and port, rather than replacing the older connection ([#6980](https://github.com/ZcashFoundation/zebra/pull/6980))
- Reduce inbound service overloads and add a timeout ([#6950](https://github.com/ZcashFoundation/zebra/pull/6950))
- Stop panicking when handling inbound connection handshakes ([#6984](https://github.com/ZcashFoundation/zebra/pull/6984))
- Stop panicking on shutdown in the syncer and network ([#7104](https://github.com/ZcashFoundation/zebra/pull/7104))

### Added

- Make the maximum number of connections per IP configurable ([#7013](https://github.com/ZcashFoundation/zebra/pull/7013))
- Make it easier to modify Zebra's config inside the Docker image ([#7045](https://github.com/ZcashFoundation/zebra/pull/7045))
- Print a Zebra logo and welcome text if stderr is terminal ([#6945](https://github.com/ZcashFoundation/zebra/pull/6945), [#7075](https://github.com/ZcashFoundation/zebra/pull/7075), [#7095](https://github.com/ZcashFoundation/zebra/pull/7095), [#7102](https://github.com/ZcashFoundation/zebra/pull/7102))

### Changed

- Move macOS to tier 3 support ([#6965](https://github.com/ZcashFoundation/zebra/pull/6965))
- Install from crates.io in the README, rather than a git release tag ([#6977](https://github.com/ZcashFoundation/zebra/pull/6977))
- Add extra timeout logging to peer TCP connections ([#6969](https://github.com/ZcashFoundation/zebra/pull/6969))

### Fixed

- Stop overwriting custom user configs inside Zebra's Docker image ([#7045](https://github.com/ZcashFoundation/zebra/pull/7045))
- Stop Zebra using 100% CPU even when idle ([#7103](https://github.com/ZcashFoundation/zebra/pull/7103)), thank you to james_katz for reporting this bug
- Avoid potential hangs in the `tokio` async runtime ([#7094](https://github.com/ZcashFoundation/zebra/pull/7094))
- Replace or add RPC content type header to support `zcashd` RPC examples ([#6885](https://github.com/ZcashFoundation/zebra/pull/6885))
- Make `zebra-network` licensing clearer ([#6995](https://github.com/ZcashFoundation/zebra/pull/6995))

#### Configuration

- Ignore error from loading config if running the 'generate' or 'download' commands ([#7014](https://github.com/ZcashFoundation/zebra/pull/7014))
- Apply force\_color to panic logs ([#6997](https://github.com/ZcashFoundation/zebra/pull/6997))

#### Logging & Error Handling

- Log a zebra-network task cancel on shutdown, rather than panicking ([#7078](https://github.com/ZcashFoundation/zebra/pull/7078))
- Fix incorrect function spans in some logs ([#6923](https://github.com/ZcashFoundation/zebra/pull/6923), [#6995](https://github.com/ZcashFoundation/zebra/pull/6995))
- Replace a state validation chain length assertion with a NotReadyToBeCommitted error ([#7072](https://github.com/ZcashFoundation/zebra/pull/7072))

#### Experimental Feature Fixes

- Add an elasticsearch feature to block serialize to fix experimental build failures ([#6709](https://github.com/ZcashFoundation/zebra/pull/6709))
- Prevent progress bar from panicking by disabling limits that are never reached ([#6940](https://github.com/ZcashFoundation/zebra/pull/6940))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @conradoplg, @dconnolly, @dimxy from komodo, james_katz, @oxarbitrage, @teor2345, @upbqdn, and the Ziggurat team.


## [Zebra 1.0.0](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0) - 2023-06-14

This is our 1.0.0 stable release.

This release also fixes a panic at startup when parsing the app version, [publishes `zebrad` to crates.io](https://crates.io/crates/zebrad), and [publishes to Docker Hub under the `latest` tag](https://hub.docker.com/r/zfnd/zebra/tags).

Please report bugs to [the Zebra GitHub repository](https://github.com/ZcashFoundation/zebra/issues/new?assignees=&labels=C-bug%2C+S-needs-triage&projects=&template=bug_report.yml&title=)

### Security

- Avoid potential concurrency bugs in outbound handshakes ([#6869](https://github.com/ZcashFoundation/zebra/pull/6869))

### Changed

- Publish to [crates.io](https://crates.io/crates/zebrad) ([#6908](https://github.com/ZcashFoundation/zebra/pull/6908))
- Rename tower-batch to tower-batch-control ([#6907](https://github.com/ZcashFoundation/zebra/pull/6907))
- Upgrade to ed25519-zebra 4.0.0 ([#6881](https://github.com/ZcashFoundation/zebra/pull/6881))

### Fixed

- Stop panicking at startup when parsing the app version ([#6888](https://github.com/ZcashFoundation/zebra/pull/6888))
- Avoid a race condition in testing modified JoinSplits ([#6921](https://github.com/ZcashFoundation/zebra/pull/6921))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@dconnolly, @gustavovalverde, @oxarbitrage, @teor2345 and @upbqdn


## [Zebra 1.0.0-rc.9](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-rc.9) - 2023-06-07

This release continues to address audit findings. It fixes multiple network protocol and RPC bugs,
and reduces sensitive information logging.

This is the last release candidate before the 1.0.0 stable release. Please report bugs to [the Zebra GitHub repository](https://github.com/ZcashFoundation/zebra/issues/new?assignees=&labels=C-bug%2C+S-needs-triage&projects=&template=bug_report.yml&title=)

### Breaking Changes

- The version subcommand has been replaced with a --version/-V flag ([#6801](https://github.com/ZcashFoundation/zebra/pull/6801))

### Security

- Stop logging peer IP addresses, to protect user privacy ([#6662](https://github.com/ZcashFoundation/zebra/pull/6662))
- Stop logging potentially sensitive user information from unmined transactions ([#6616](https://github.com/ZcashFoundation/zebra/pull/6616))
- Rate-limit MetaAddrChange::Responded from peers ([#6738](https://github.com/ZcashFoundation/zebra/pull/6738))
- Ignore out of order Address Book changes, unless they are concurrent ([#6717](https://github.com/ZcashFoundation/zebra/pull/6717))
- Limit blocks and transactions sent in response to a single request  ([#6679](https://github.com/ZcashFoundation/zebra/pull/6679))
- Rate-limit and size-limit peer transaction ID messages ([#6625](https://github.com/ZcashFoundation/zebra/pull/6625))
- Stop panicking on state RPC or block requests with very large heights ([#6699](https://github.com/ZcashFoundation/zebra/pull/6699))
- Try harder to drop connections when they shut down, Credit: Ziggurat Team ([#6832](https://github.com/ZcashFoundation/zebra/pull/6832))
- Randomly drop connections when inbound service is overloaded ([#6790](https://github.com/ZcashFoundation/zebra/pull/6790))

### Added

- Report compiler version and Zebra features when starting Zebra ([#6606](https://github.com/ZcashFoundation/zebra/pull/6606))
- Update Zebra book summary to include supported platforms, platform tier policy, and versioning ([#6683](https://github.com/ZcashFoundation/zebra/pull/6683))
- Improve zebrad's help output, credit to @Rqnsom ([#6801](https://github.com/ZcashFoundation/zebra/pull/6801))
- Cache a list of useful peers on disk ([#6739](https://github.com/ZcashFoundation/zebra/pull/6739))
- Make the first stable release forward-compatible with planned state changes ([#6813](https://github.com/ZcashFoundation/zebra/pull/6813))

### Fixed

- Limit RPC failure log length, add details to RPC failure logs ([#6754](https://github.com/ZcashFoundation/zebra/pull/6754))
- Allow inbound connections to Zebra running in Docker ([#6755](https://github.com/ZcashFoundation/zebra/pull/6755))
- Zebra now accepts filters for the start command when no subcommand is provided ([#6801](https://github.com/ZcashFoundation/zebra/pull/6801))
- Avoid panicking on state errors during shutdown ([#6828](https://github.com/ZcashFoundation/zebra/pull/6828))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @mpguerra, @oxarbitrage, @teor2345 and @upbqdn


## [Zebra 1.0.0-rc.8](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-rc.8) - 2023-05-10

Starting in this release, Zebra has implemented an "end of support" halt. Just like `zcashd`, the `zebrad` binary will stop running 16 weeks after the last release date.
Also, this release adds the ZIP-317 rules to mempool transactions which should help with the Zcash network spam issue.

### Security

- Avoid inbound service overloads and fix failing tests ([#6537](https://github.com/ZcashFoundation/zebra/pull/6537))
- Avoid a rare panic when a connection is dropped ([#6566](https://github.com/ZcashFoundation/zebra/pull/6566))
- Avoid some self-connection nonce removal attacks ([#6410](https://github.com/ZcashFoundation/zebra/pull/6410))
- Reject nodes using ZClassic ports, and warn if configured with those ports ([#6567](https://github.com/ZcashFoundation/zebra/pull/6567))

### Added

- Add ZIP-317 rules to mempool ([#6556](https://github.com/ZcashFoundation/zebra/pull/6556))
- Add user agent argument to zebra-network crate ([#6601](https://github.com/ZcashFoundation/zebra/pull/6601))
- Refuse to run zebrad when release is too old ([#6351](https://github.com/ZcashFoundation/zebra/pull/6351))

### Fixed

- Handle randomness generation and invalid random values as errors in cryptographic code ([#6385](https://github.com/ZcashFoundation/zebra/pull/6385))
- When configured for testnet, automatically use the correct testnet listener port ([#6575](https://github.com/ZcashFoundation/zebra/pull/6575))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @gustavovalverde, @oxarbitrage, @teor2345 and @upbqdn


## [Zebra 1.0.0-rc.7](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-rc.7) - 2023-04-18

This release features a security fix for unbounded memory use in zebra-network, introduces the "progress-bar" feature, and continues to address audit findings.

### Security

- Limit the number of leftover nonces in the self-connection nonce set ([#6534](https://github.com/ZcashFoundation/zebra/pull/6534))
- Allow each initial peer to send one inbound request before disconnecting any peers ([#6520](https://github.com/ZcashFoundation/zebra/pull/6520))
- Limit the number of non-finalized chains tracked by Zebra ([#6447](https://github.com/ZcashFoundation/zebra/pull/6447))
- Update dependencies that only appear in the lock file ([#6217](https://github.com/ZcashFoundation/zebra/pull/6217))

### Added

- Add confirmations to getrawtransaction method response ([#6287](https://github.com/ZcashFoundation/zebra/pull/6287))
- Add a config for writing logs to a file ([#6449](https://github.com/ZcashFoundation/zebra/pull/6449))
- Add an experimental terminal-based progress bar feature to Zebra, which is off by default ([#6235](https://github.com/ZcashFoundation/zebra/pull/6235))
- Create DockerHub image with mining enabled after each Zebra release ([#6228](https://github.com/ZcashFoundation/zebra/pull/6228))

### Changed

- Increase ZIP-401 mempool cost thresholds for Orchard transactions ([#6521](https://github.com/ZcashFoundation/zebra/pull/6521))
- Suggest making sure the RPC endpoint is enabled for checkpointing ([#6375](https://github.com/ZcashFoundation/zebra/pull/6375))
- Refactor the handling of height differences ([#6330](https://github.com/ZcashFoundation/zebra/pull/6330))
- Upgrade shared dependencies to match `zcashd` 5.5.0 ([#6536](https://github.com/ZcashFoundation/zebra/pull/6536))
- Lookup unspent UTXOs in non-finalized state before checking disk ([#6513](https://github.com/ZcashFoundation/zebra/pull/6513))
- Stop re-downloading blocks that are in non-finalized side chains ([#6335](https://github.com/ZcashFoundation/zebra/pull/6335))

### Fixed

- Validate header versions when serializing blocks ([#6475](https://github.com/ZcashFoundation/zebra/pull/6475))
- Stop ignoring new transactions after the mempool is newly activated ([#6448](https://github.com/ZcashFoundation/zebra/pull/6448))
- Fix off-by-one error in DNS seed peer retries, and clarify logs ([#6460](https://github.com/ZcashFoundation/zebra/pull/6460))
- Check that mempool transactions are valid for the state's chain info in getblocktemplate ([#6416](https://github.com/ZcashFoundation/zebra/pull/6416))
- Remove transactions with immature transparent coinbase spends from the mempool and block templates ([#6510](https://github.com/ZcashFoundation/zebra/pull/6510))
- Disable issue URLs for a known shutdown panic in abscissa ([#6486](https://github.com/ZcashFoundation/zebra/pull/6486))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @dconnolly, @gustavovalverde, @oxarbitrage, @teor2345 and @upbqdn

## [Zebra 1.0.0-rc.6](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-rc.6) - 2023-03-23

In this release, we fixed several minor security issues, most notably [hardening Zebra in response to the vulnerabilities recently disclosed by Halborn](https://zfnd.org/statement-on-recent-security-disclosures-by-halborn/).

### Known Issues

- `orchard` 0.3.0 can't verify `halo2` proofs when compiled with Rust 1.69 or later (currently beta and nightly Rust). Compile Zebra with stable Rust to avoid this bug. ([halo2/#737](https://github.com/zcash/halo2/issues/737)).
Zebra tracking issue for this problem is [#6232](https://github.com/ZcashFoundation/zebra/issues/6232).

### Security

- Harden Zebra's network protocol implementation in response to the Halborn disclosures ([#6297](https://github.com/ZcashFoundation/zebra/pull/6297))
- Bump incrementalmerkletree from 0.3.0 to 0.3.1, resolving a consensus bug on 32-bit platforms ([#6258](https://github.com/ZcashFoundation/zebra/pull/6258))
- Remove unused dependencies, and check for them in CI ([#6216](https://github.com/ZcashFoundation/zebra/pull/6216))
- Validate address length before reading ([#6320](https://github.com/ZcashFoundation/zebra/pull/6320), [#6368](https://github.com/ZcashFoundation/zebra/pull/6368))

### Added

- Add instructions for mining with s-nomp to Zebra book ([#6220](https://github.com/ZcashFoundation/zebra/pull/6220))
- Export block data to elasticsearch database ([#6274](https://github.com/ZcashFoundation/zebra/pull/6274))
- Add elasticsearch section to Zebra book ([#6295](https://github.com/ZcashFoundation/zebra/pull/6295))

### Changed

- Update Zebra's build instructions ([#6273](https://github.com/ZcashFoundation/zebra/pull/6273))
- Improve CommitBlockError message ([#6251](https://github.com/ZcashFoundation/zebra/pull/6251))

### Fixed

- Stop ignoring some transaction broadcasts ([#6230](https://github.com/ZcashFoundation/zebra/pull/6230))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @dconnolly, @mpguerra, @oxarbitrage, @teor2345 and @upbqdn


## [Zebra 1.0.0-rc.5](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-rc.5) - 2023-02-23

This release:

- finishes the implementation of mining-related RPCs;
- makes Zebra ready for testing and initial adoption by mining pools;
- adds support for the latest version of `lightwalletd`;
- makes non-finalized chain forks instant;
- contains other improvements such as deduplication of RedPallas code.

Zebra now also checks that it is following the consensus chain each time it
starts up.

### Security

- Check that Zebra's state contains the consensus chain each time it starts up.
  This implements the "settled network upgrade" consensus rule using all of
  Zebra's checkpoints
  ([#6163](https://github.com/ZcashFoundation/zebra/pull/6163)).

  _User action required:_

  - If your config is based on an old version of Zebra, or you have manually
    edited it, make sure `consensus.checkpoint_sync = true`. This option has
    been true by default since March 2022.

- Bump hyper from 0.14.23 to 0.14.24, fixing a denial of service risk ([#6094](https://github.com/ZcashFoundation/zebra/pull/6094))
- Re-verify transactions that were verified at a different tip height ([#6154](https://github.com/ZcashFoundation/zebra/pull/6154))
- Fix minute-long delays in block verification after a chain fork ([#6122](https://github.com/ZcashFoundation/zebra/pull/6122))

### Deprecated

- The `consensus.checkpoint_sync` config in `zebrad.toml` is deprecated. It might be ignored or
  removed in a future release. ([#6163](https://github.com/ZcashFoundation/zebra/pull/6163))

### Added

- Log a cute message for blocks that were mined by Zebra (off by default) ([#6098](https://github.com/ZcashFoundation/zebra/pull/6098))
- Add extra `getblock` RPC fields used by some mining pools ([#6097](https://github.com/ZcashFoundation/zebra/pull/6097))
- Get details from transaction differences in `getrawmempool` RPC ([#6035](https://github.com/ZcashFoundation/zebra/pull/6035))

#### New RPCs

- Implement the `z_listunifiedreceivers` RPC ([#6171](https://github.com/ZcashFoundation/zebra/pull/6171))
- Implement the `getblocksubsidy` RPC ([#6032](https://github.com/ZcashFoundation/zebra/pull/6032))
- Implement the `validateaddress` RPC ([#6086](https://github.com/ZcashFoundation/zebra/pull/6086))
- Implement the `z_validateaddress` RPC ([#6185](https://github.com/ZcashFoundation/zebra/pull/6185))
- Implement the `getdifficulty` RPC ([#6099](https://github.com/ZcashFoundation/zebra/pull/6099))

#### Documentation

- Add detailed testnet mining docs to the Zebra repository ([#6201](https://github.com/ZcashFoundation/zebra/pull/6201))
- Add mining instructions to the zebra book ([#6199](https://github.com/ZcashFoundation/zebra/pull/6199))

### Changed

- Use `reddsa` crate and remove duplicated RedPallas code ([#6013](https://github.com/ZcashFoundation/zebra/pull/6013))
- Upgrade to the zcash_primitives 0.10 API ([#6087](https://github.com/ZcashFoundation/zebra/pull/6087))
- Simplify `getdifficulty` RPC implementation ([#6105](https://github.com/ZcashFoundation/zebra/pull/6105))

### Fixed

- Change error format in proposals ([#6044](https://github.com/ZcashFoundation/zebra/pull/6044))
- Fix `lightwalletd` instructions to be compatible with Zebra ([#6088](https://github.com/ZcashFoundation/zebra/pull/6088))
- Downgrade `owo-colors` from 3.6.0 to 3.5.0 ([#6203](https://github.com/ZcashFoundation/zebra/pull/6203))
- Make RPC "incorrect parameters" error code match `zcashd` ([#6066](https://github.com/ZcashFoundation/zebra/pull/6066))
- Make the verbosity argument optional in the getblock RPC ([#6092](https://github.com/ZcashFoundation/zebra/pull/6092))
- Increase legacy chain limit to 100,000 ([#6053](https://github.com/ZcashFoundation/zebra/pull/6053))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra
without you: @arya2, @conradoplg, @gustavovalverde,
@jackgavigan, @oxarbitrage, @sandakersmann, @teor2345 and @upbqdn

## [Zebra 1.0.0-rc.4](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-rc.4) - 2023-01-30

In this release we fixed bugs and inconsistencies between zcashd and zebrad in the output of the `getblocktemplate` RPC method. In addition, we added block proposal mode to the `getblocktemplate` RPC, while we continue the effort of adding and testing mining pool RPC methods.

### Security

- Verify the lock times of mempool transactions. Previously, Zebra was ignoring mempool transaction lock times, but checking them in mined blocks. Credit to DeckerSU for reporting this issue. ([#6027](https://github.com/ZcashFoundation/zebra/pull/6027))
- Bump bumpalo from 3.8.0 to 3.12.0, removing undefined behaviour on `wasm` targets. These targets are not supported Zebra platforms. ([#6015](https://github.com/ZcashFoundation/zebra/pull/6015))
- Bump libgit2-sys from 0.14.0+1.5.0 to 0.14.2+1.5.1, to ensure that SSH server keys are checked. Zebra only uses `libgit2` during builds, and we don't make SSH connections. ([#6014](https://github.com/ZcashFoundation/zebra/pull/6014))
- Bump tokio from 1.24.1 to 1.24.2, to fix unsoundness. The unsound methods are not directly used by Zebra. ([#5995](https://github.com/ZcashFoundation/zebra/pull/5995))

### Added

- Add getpeerinfo RPC method ([#5951](https://github.com/ZcashFoundation/zebra/pull/5951))
- Add proposal capability to getblocktemplate ([#5870](https://github.com/ZcashFoundation/zebra/pull/5870))
- Add a test to check that the Docker image config works ([#5968](https://github.com/ZcashFoundation/zebra/pull/5968))
- Make `zebra-checkpoints` work for zebrad backend ([#5894](https://github.com/ZcashFoundation/zebra/pull/5894), [#5961](https://github.com/ZcashFoundation/zebra/pull/5961))
- Add test dependency from zebra-rpc to zebra-network with correct features ([#5992](https://github.com/ZcashFoundation/zebra/pull/5992))
- Document zebra download command ([#5901](https://github.com/ZcashFoundation/zebra/pull/5901))

### Fixed

- Return detailed errors to the RPC client when a block proposal fails ([#5993](https://github.com/ZcashFoundation/zebra/pull/5993))
- Avoid selecting duplicate transactions in block templates ([#6006](https://github.com/ZcashFoundation/zebra/pull/6006))
- Calculate getblocktemplate RPC testnet min and max times correctly ([#5925](https://github.com/ZcashFoundation/zebra/pull/5925))
- Fix Merkle root transaction order in getblocktemplate RPC method ([#5953](https://github.com/ZcashFoundation/zebra/pull/5953))

### Changed

- Strings in zebra configuration file now use double quotes, caused by upgrading the `toml` crate. Old configs will still work [#6029](https://github.com/ZcashFoundation/zebra/pull/6029)

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @conradoplg, @gustavovalverde, @mpguerra, @oxarbitrage and @teor2345


## [Zebra 1.0.0-rc.3](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-rc.3) - 2023-01-10

This release continues our work on mining pool RPCs, and brings Zebra up to date with the latest [ZIP-317](https://zips.z.cash/zip-0317) changes. It also fixes a minor network protocol compatibility bug.

As part of this release, we upgraded `tokio` to fix potential hangs and performance issues. We encourage all users to upgrade to the latest Zebra version to benefit from these fixes.

### Breaking Changes

- Zebra now requires at least Rust 1.65, because we have started using new language features.
  Any Zebra release can increase the required Rust version: only the latest stable Rust version is supported.

### Security

- Upgrade tokio from 1.22.0 to 1.23.0 to fix potential hangs and performance issues ([#5802](https://github.com/ZcashFoundation/zebra/pull/5802))
- Refactor block subsidy to handle Height::MAX without panicking ([#5787](https://github.com/ZcashFoundation/zebra/pull/5787))
- Update ZIP-317 transaction selection algorithm in the `getblocktemplate` RPC ([#5776](https://github.com/ZcashFoundation/zebra/pull/5776))

### Added

- Add the `getmininginfo`, `getnetworksolps` and `getnetworkhashps` RPC methods ([#5808](https://github.com/ZcashFoundation/zebra/pull/5808))
- Add long polling support to the `getblocktemplate` RPC ([#5772](https://github.com/ZcashFoundation/zebra/pull/5772), [#5796](https://github.com/ZcashFoundation/zebra/pull/5796), [#5837](https://github.com/ZcashFoundation/zebra/pull/5837), [#5843](https://github.com/ZcashFoundation/zebra/pull/5843), [#5862](https://github.com/ZcashFoundation/zebra/pull/5862))
- Populate `blockcommitmenthash` and `defaultroot` fields in the getblocktemplate RPC ([#5751](https://github.com/ZcashFoundation/zebra/pull/5751))
- Support transparent p2pkh miner addresses in the `getblocktemplate` RPC ([#5827](https://github.com/ZcashFoundation/zebra/pull/5827))

### Changed

- Automatically re-verify mempool transactions after a chain fork, rather than re-downloading them all ([#5841](https://github.com/ZcashFoundation/zebra/pull/5841))
- Try to match `zcashd`'s `getblocktemplate` exactly ([#5867](https://github.com/ZcashFoundation/zebra/pull/5867))
- Accept a hash or a height as the first parameter of the `getblock` RPC ([#5861](https://github.com/ZcashFoundation/zebra/pull/5861))
- Wait for 3 minutes to check Zebra is synced to the tip, rather than 2 ([#5840](https://github.com/ZcashFoundation/zebra/pull/5840))
- Update mainnet and testnet checkpoints ([#5928](https://github.com/ZcashFoundation/zebra/pull/5928))

### Fixed

- Allow peers to omit the `relay` flag in `version` messages ([#5835](https://github.com/ZcashFoundation/zebra/pull/5835))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @dconnolly, @dependabot[bot], @oxarbitrage and @teor2345


## [Zebra 1.0.0-rc.2](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-rc.2) - 2022-12-06

Zebra's latest release continues work on mining pool RPCs, fixes a rare RPC crash that could lead to memory corruption, and uses the ZIP-317 conventional fee for mempool size limits.

Zebra's consensus rules, node sync, and `lightwalletd` RPCs are ready for user testing and experimental use. Zebra has not been audited yet.

### Breaking Changes

This release has the following breaking changes:
- Evict transactions from the mempool using the ZIP-317 conventional fee ([#5703](https://github.com/ZcashFoundation/zebra/pull/5703))
  - If there are a lot of unmined transactions on the Zcash network, and Zebra's mempool
    becomes full, Zebra will penalise transactions that don't pay at least the ZIP-317
    conventional fee. These transactions will be more likely to get evicted.
  - The ZIP-317 convention fee increases based on the number of logical transparent or
    shielded actions in a transaction.
  - This change has no impact under normal network conditions.

### Security

- Fix a rare crash and memory errors when Zebra's RPC server shuts down ([#5591](https://github.com/ZcashFoundation/zebra/pull/5591))
- Evict transactions from the mempool using the ZIP-317 conventional fee ([#5703](https://github.com/ZcashFoundation/zebra/pull/5703))

### Added

- Add submitblock RPC method ([#5526](https://github.com/ZcashFoundation/zebra/pull/5526))
- Add a `mining` section with miner address to config ([#5491](https://github.com/ZcashFoundation/zebra/pull/5508))

### Changed

- Select getblocktemplate RPC transactions according to ZIP-317 ([#5724](https://github.com/ZcashFoundation/zebra/pull/5724))
- Add transaction fields to the `getblocktemplate` RPC ([#5496](https://github.com/ZcashFoundation/zebra/pull/5496) and [#5508](https://github.com/ZcashFoundation/zebra/pull/5508))
- Populate some getblocktemplate RPC block header fields using the state best chain tip  ([#5659](https://github.com/ZcashFoundation/zebra/pull/5659))
- Return an error from getblocktemplate method if Zebra is not synced to network tip ([#5623](https://github.com/ZcashFoundation/zebra/pull/5623))
- Implement coinbase conversion to RPC `TransactionTemplate` type ([#5554](https://github.com/ZcashFoundation/zebra/pull/5554))
- Check block and transaction Sprout anchors in parallel ([#5742](https://github.com/ZcashFoundation/zebra/pull/5742))
- Contextually validates mempool transactions in best chain ([#5716](https://github.com/ZcashFoundation/zebra/pull/5716) and [#5616](https://github.com/ZcashFoundation/zebra/pull/5616))
- Generate coinbase transactions in the getblocktemplate RPC ([#5580](https://github.com/ZcashFoundation/zebra/pull/5580))
- Log loaded config path when Zebra starts up ([#5733](https://github.com/ZcashFoundation/zebra/pull/5733))
- Update mainnet and testnet checkpoints on 2022-12-01 ([#5754](https://github.com/ZcashFoundation/zebra/pull/5754))
- Bump zcash\_proofs from 0.8.0 to 0.9.0 and zcash\_primitives from 0.8.1 to 0.9.0 ([#5631](https://github.com/ZcashFoundation/zebra/pull/5631))

### Fixed

- Check network and P2SH addresses for mining config and funding streams([#5620](https://github.com/ZcashFoundation/zebra/pull/5620))
- Return an error instead of panicking in the batch verifier on shutdown ([#5530](https://github.com/ZcashFoundation/zebra/pull/5530))
- Use a more reliable release template branch name and docker command ([#5519](https://github.com/ZcashFoundation/zebra/pull/5519))
- Make the syncer ignore some new block verification errors ([#5537](https://github.com/ZcashFoundation/zebra/pull/5537))
- Pause new downloads when Zebra reaches the lookahead limit ([#5561](https://github.com/ZcashFoundation/zebra/pull/5561))
- Shut down the RPC server properly when Zebra shuts down ([#5591](https://github.com/ZcashFoundation/zebra/pull/5591))
- Print usage info for --help flag ([#5634](https://github.com/ZcashFoundation/zebra/pull/5634))
- Fix RPC bugs ([#5761](https://github.com/ZcashFoundation/zebra/pull/5761))
- Clarify inbound and outbound port requirements ([#5584](https://github.com/ZcashFoundation/zebra/pull/5584))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @oxarbitrage, @teor2345, and @mpguerra


## [Zebra 1.0.0-rc.1](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-rc.1) - 2022-11-02

This is the second Zebra release candidate. Zebra's consensus rules, node sync, and `lightwalletd` RPCs are ready for user testing and experimental use. Zebra has not been audited yet.

This release starts work on mining pool RPCs, including some mempool fixes. It also restores support for Rust 1.64.

### Breaking Changes

This release has the following breaking changes:
- Remove unused buggy cryptographic code from zebra-chain ([#5464](https://github.com/ZcashFoundation/zebra/pull/5464), [#5476](https://github.com/ZcashFoundation/zebra/pull/5476)). This code was never used in production, and it had known bugs. Anyone using it should migrate to `librustzcash` instead.

### Added

- Introduce `getblocktemplate-rpcs` feature ([#5357](https://github.com/ZcashFoundation/zebra/pull/5357))
  - Add getblockcount rpc method ([#5357](https://github.com/ZcashFoundation/zebra/pull/5357))
  - Add getblockhash rpc method ([#4967](https://github.com/ZcashFoundation/zebra/pull/4967))
  - Add getblocktemplate rpc call with stub fields ([#5462](https://github.com/ZcashFoundation/zebra/pull/5462))
- Add block commit task metrics ([#5327](https://github.com/ZcashFoundation/zebra/pull/5327))
- Document how we tag and release Zebra ([#5392](https://github.com/ZcashFoundation/zebra/pull/5392))
- Document how to use Zebra with Docker ([#5504](https://github.com/ZcashFoundation/zebra/pull/5504))

### Changed

- Update mainnet and testnet checkpoints ([#5512](https://github.com/ZcashFoundation/zebra/pull/5512))

### Fixed

- Reject mempool transactions with spent outpoints or nullifiers ([#5434](https://github.com/ZcashFoundation/zebra/pull/5434))
- Allow extra lookahead blocks in the verifier, state, and block commit task queues. This reduces the number of downloaded blocks that are dropped due to the lookahead limit. ([#5465](https://github.com/ZcashFoundation/zebra/pull/5465))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @gustavovalverde, @oxarbitrage, @teor2345 and @upbqdn


## [Zebra 1.0.0-rc.0](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-rc.0) - 2022-10-12

This is the first Zebra release candidate. Zebra's consensus rules, node sync, and `lightwalletd` RPCs are ready for user testing and experimental use. Zebra has not been audited yet.

This release also makes significant performance improvements to RPCs, and temporarily removes support for Rust 1.64.

### Breaking Changes

This release has the following breaking changes:
- Rust 1.64 is unsupported due to a performance regression when downloading the Zcash parameters.
  Zebra currently builds with Rust 1.63 ([#5251](https://github.com/ZcashFoundation/zebra/pull/5251)).
- Use correct TOML syntax in Docker zebrad.toml ([#5320](https://github.com/ZcashFoundation/zebra/pull/5320))

### Major RPC Performance Improvements

This release improves RPC performance:
- Initial `lightwalletd` sync is about twice as fast ([#5307](https://github.com/ZcashFoundation/zebra/pull/5307))
- RPCs can run while a block is being committed to the state, previously they could be delayed by 5-15 seconds ([#5134](https://github.com/ZcashFoundation/zebra/pull/5134), [#5257](https://github.com/ZcashFoundation/zebra/pull/5257))

### Security

- Make default command work in docker images, disable optional listener ports ([#5313](https://github.com/ZcashFoundation/zebra/pull/5313))
- Update yanked versions of cpufeatures (orchard/aes), ed25519 (tor), and quick-xml (flamegraph) ([#5308](https://github.com/ZcashFoundation/zebra/pull/5308))
- Bump rand\_core from 0.6.3 to 0.6.4, fixes unsoundness bug ([#5175](https://github.com/ZcashFoundation/zebra/pull/5175))

### Changed

- Log git metadata and platform info when zebrad starts up ([#5200](https://github.com/ZcashFoundation/zebra/pull/5200))
- Update mainnet and testnet checkpoints ([#5360](https://github.com/ZcashFoundation/zebra/pull/5360))
- Update README for the release candidate ([#5314](https://github.com/ZcashFoundation/zebra/pull/5314))
- change(docs): Add links to CI/CD docs in the zebra book's sidebar ([#5355](https://github.com/ZcashFoundation/zebra/pull/5355))

### Fixed

- Use correct TOML syntax in Docker zebrad.toml ([#5320](https://github.com/ZcashFoundation/zebra/pull/5320))
- Look back up to 10,000 blocks on testnet for a legacy chain ([#5133](https://github.com/ZcashFoundation/zebra/pull/5133))

#### Performance

- Build zebrad with Rust 1.63 to avoid Zcash parameter download hangs ([#5251](https://github.com/ZcashFoundation/zebra/pull/5251))
- Write blocks to the state in a separate thread, to avoid network and RPC hangs ([#5134](https://github.com/ZcashFoundation/zebra/pull/5134), [#5257](https://github.com/ZcashFoundation/zebra/pull/5257))
- Fix slow getblock RPC (verbose=1) using transaction ID index ([#5307](https://github.com/ZcashFoundation/zebra/pull/5307))
- Open the database in a blocking tokio thread, which allows tokio to run other tasks  ([#5228](https://github.com/ZcashFoundation/zebra/pull/5228))

### Contributors

Thank you to everyone who contributed to this release, we couldn't make Zebra without you:
@arya2, @gustavovalverde, @oxarbitrage, @teor2345 and @upbqdn.

## [Zebra 1.0.0-beta.15](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.15) - 2022-09-20

This release improves Zebra's sync and RPC performance, improves test coverage and reliability, and starts creating Docker Hub binaries for releases.

It also changes some of `zebra-network`'s unstable Rust APIs to provide more peer metadata.

### Breaking Changes

This release has the following breaking changes:
- Zebra's JSON-RPC server has an isolated thread pool, which is single threaded by default (#4806).
  Multi-threaded RPCs disable port conflict detection, allowing multiple Zebra instances to share
  the same RPC port (#5013).
  To activate multi-threaded RPC server requests, add this config to your `zebrad.toml`:

```toml
[rpc]
parallel_cpu_threads = 0
```

- Docker users can now specify a custom `zebrad` config file path (#5163, #5177).
  As part of this feature, the default config path in the Docker image was changed.

- `zebrad` now uses a non-blocking tracing logger (#5032).
  If the log buffer fills up, Zebra will discard any additional logs.
  Moving logging to a separate thread also changes the timing of some logs,
  this is unlikely to affect most users.

- Zebra's gRPC tests need `protoc` installed (#5009).
  If you are running Zebra's `lightwalletd` gRPC test suite, [see the `tonic` README for details.](https://github.com/hyperium/tonic#dependencies)

### Added

#### Releasing Zebra

- Create Docker hub binaries when tagging releases (#5138)
- Document how Zebra is versioned and released (#4917, #5026)
- Allow manual Release Drafter workflow runs (#5165)

#### Network Peer Rust APIs

Note: Zebra's Rust APIs are unstable, they are not covered by semantic versioning.

- Return peer metadata from `connect_isolated` functions (#4870)

#### Testing

- Check that the latest Zebra version can parse previous configs (#5112)
- Test disabled `lightwalletd` mempool gRPCs via zebrad logs (#5016)

#### Documentation

- Document why Zebra does a UTXO check that looks redundant (#5106)
- Document how Zebra CI works (#5038, #5080, #5100, #5105, #5017)

### Changed

#### Zebra JSON-RPCs

- Breaking: Add a config for multi-threaded RPC server requests (#5013)
- Isolate RPC queries from the rest of Zebra, to improve performance (#4806)

#### Zebra State

- Send treestates from non-finalized state to finalized state, rather than re-calculating them (#4721)
- Run StateService read requests without shared mutable chain state (#5132, #5107)
- Move the finalized block queue to the StateService (#5152)
- Simplify StateService request processing and metrics tracking (#5137)

#### Block Checkpoints

- Update Zebra checkpoints (#5130)

#### Docker Images

- Breaking: Allow Docker users to specify a custom `zebrad` config file path (#5163, #5177)

#### Continuous Integration and Deployment

- Wait 1 day before creating cached state image updates  (#5088)
- Delete cached state images older than 2 days, but keep a few recent images
  (#5113, #5124, #5082, #5079)
- Simplify GitHub actions caches (#5104)
- Use 200GB disks for managed instances (#5084)
- Improve test reliability and test output readability (#5014)

#### Zebra Dependencies

- Breaking: Update prost, tonic, tonic-build and console-subscriber to the latest versions (#5009)
- Upgrade `zcash\_script` and other shared zcash dependencies (#4926)
- Update deny.toml developer docs and file comments (#5151, #5070)
- Update other Zebra Rust dependencies to the latest versions
  (#5150, #5160, #5176, #5149, #5136, #5135, #5118, #5009, #5074, #5071, #5037, #5020, #4914,
  #5057, #5058, #5047, #4984, #5021, #4998, #4916, #5022, #4912, #5018, #5019)

#### CI Dependencies

- Update Zebra CI dependencies to the latest versions (#5159, #5148, #5117, #5073, #5029)

### Removed

#### Continuous Integration

- Disable beta Rust tests (#5090, #5024)

### Fixed

#### Logging

- Breaking: Switch `zebrad` to a non-blocking tracing logger (#5032)

#### Testing

- Increase full sync timeout to 32 hours (#5172, #5129)
- Disable unreliable RPC port conflict tests on Windows and macOS (#5072)
- Increase slow code log thresholds to reduce verbose logs and warnings (#4997)

#### Continuous Integration

- Fix full sync CI failures by adding an extra GitHub job (#5166)
- Make checkpoint disk image names short enough for Google Cloud (#5128)
- Label lightwalletd cached state images with their sync height (#5086)

#### Lints

- Fix various Rust clippy lints (#5131, #5045)
- Fix a failure in `tj-actions/changed-files` on push (#5097)

#### Documentation

- Enable all cargo features in Zebra's deployed documentation (#5156)

### Security

#### JSON-RPC Server

- Isolate RPC queries from the rest of Zebra,
  so that `lightwalletd` clients are more isolated from each other (#4806)


## [Zebra 1.0.0-beta.14](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.14) - 2022-08-30

This release contains a variety of CI fixes, test fixes and dependency updates.
It contains two breaking changes:

- the recommended disk capacity for Zebra is now 300 GB, and the recommended network bandwidth is 100 GB per month, and
- when no command is provided on the command line, `zebrad` automatically starts syncing (like `zcashd`).

The sync performance of `lightwalletd` is also improved.

### Added

- Store history trees by height in the non-finalized state (#4928)
- Breaking: Add `start` as default subcommand for `zebrad` (#4957)

### Changed

- Fix a performance regression when serving blocks via the Zcash network protocol and RPCs (#4933)
- Update block hash checkpoints for mainnet (#4919, #4972)
- Enable a `tinyvec` feature to speed up compilation (#4796)
- Split the `zebra_state::service::read` module (#4827)
- Disallow Orchard `ivk = 0` on `IncomingViewingKey::from` & `SpendingKey` generation (#3962)

#### Docs

- Increase disk and network requirements for long-term deployment (#4948, #4963)
- Update supported Rust versions in `README.md` (#4938)
- Document edge cases in sync workflows (#4973)
- Add missing status badges & sections (#4817)

#### Rust Dependencies

- Bump `serde` from 1.0.137 to 1.0.144 (#4865, #4876, #4925)
- Bump `serde_json` from 1.0.81 to 1.0.83 (#4727, #4877)
- Bump `serde_with` from 1.14.0 to 2.0.0 (#4785)
- Bump `futures` from 0.3.21 to 0.3.23 (#4913)
- Bump `futures-core` from 0.3.21 to 0.3.23 (#4915)
- Bump `chrono` from 0.4.19 to 0.4.20 (#4898)
- Bump `criterion` from 0.3.5 to 0.3.6 (#4761)
- Bump `thiserror` from 1.0.31 to 1.0.32 (#4878)
- Bump `vergen` from 7.2.1 to 7.3.2 (#4890)
- Bump `tinyvec` from 1.5.1 to 1.6.0 (#4888)
- Bump `insta` from 1.15.0 to 1.17.1 (#4884)
- Bump `semver` from 1.0.12 to 1.0.13 (#4879)
- Bump `bytes` from 1.1.0 to 1.2.1 (#4843)
- Bump `tokio` from 1.20.0 to 1.20.1 (#4864)
- Bump `hyper` from 0.14.19 to 0.14.20 (#4764)
- Bump `once_cell` from 1.12.0 to 1.13.0 (#4749)
- Bump `regex` from 1.5.6 to 1.6.0 (#4755)
- Bump `inferno` from 0.11.6 to 0.11.7 (#4829)

#### CI Dependencies

- Bump `actions/github-script` from 6.1.0 to 6.2.0 (#4986)
- Bump `reviewdog/action-actionlint` from 1.27.0 to 1.29.0 (#4923, #4987)
- Bump `tj-actions/changed-files` from 24 to 29.0.2 (#4936, #4959, #4985)
- Bump `w9jds/firebase-action` from 2.2.2 to 11.5.0 (#4905)
- Bump `docker/build-push-action` from 3.0.0 to 3.1.1 (#4797, #4895)

### Fixed

- Increase the number of blocks checked for legacy transactions (#4804)

#### CI

- Split a long full sync job (#5001)
- Stop cancelling manual full syncs (#5000)
- Run a single CI workflow as required (#4981)
- Fix some clippy warnings (#4927, #4931)
- Improve Zebra acceptance test diagnostics (#4958)
- Expand cached state disks before running tests (#4962)
- Increase full sync timeouts for longer syncs (#4961)
- Fix a regular expression typo in a full sync job (#4950)
- Write cached state images after update syncs, and use the latest image from any commit (#4949)
- Increase CI disk size to 200GB (#4945)
- Make sure Rust tests actually run in `deploy-gcp-tests.yml` (#4710)
- Copy lightwalletd from the correct path during Docker builds (#4886)
- Use FHS for deployments and artifacts (#4786)
- Retry gcloud authentication if it fails (#4940)
- Disable beta Rust tests and add parameter download logging (#4930)
- Do not run versioning job when pushing to main (#4970)
- Deploy long running node instances on release (#4939)
- Run build and test jobs on cargo and clippy config changes (#4941)
- Increase Mergify batch sizes (#4947)

#### Networking

- Send height to peers (#4904)
- Fix handshake timing and error handling (#4772)

#### Tests

- Show full Zebra test panic details in CI logs (#4942)
- Update timeout for Zebra sync tests (#4918)
- Improve test reliability and performance (#4869)
- Use `FuturesOrdered` in `fallback_verification` test (#4867)
- Skip some RPC tests when `SKIP_NETWORK_TESTS` is set (#4849)
- Truncate the number of transactions in send transaction test (#4848)

## [Zebra 1.0.0-beta.13](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.13) - 2022-07-29

This release fixes multiple bugs in proof and signature verification, which were causing big performance issues near the blockchain tip.
It also improves Zebra's sync performance and reliability under heavy load.

### Disk and Network Usage Changes

Zebra now uses around 50 - 100 GB of disk space, because many large transactions were recently added to the block chain. (In the longer term, several hundred GB are likely to be needed.)

When there are a lot of large user-generated transactions on the network, Zebra can upload or download 1 GB or more per day.

### Configuration Changes

- Split the checkpoint and full verification [`sync` concurrency options](https://docs.rs/zebrad/latest/zebrad/components/sync/struct.Config.html) (#4726, #4758):
  - Add a new `full_verify_concurrency_limit`
  - Rename `max_concurrent_block_requests` to `download_concurrency_limit`
  - Rename `lookahead_limit` to `checkpoint_verify_concurrency_limit`
  For backwards compatibility, the old names are still accepted as aliases.
- Add a new `parallel_cpu_threads` [`sync` concurrency option](https://docs.rs/zebrad/latest/zebrad/components/sync/struct.Config.html) (#4776).
  This option sets the number of threads to use for CPU-bound tasks, such as proof and signature verification.
  By default, Zebra uses all available CPU cores.

### Rust Compiler Bug Fixes

- The Rust team has recently [fixed compilation bugs](https://blog.rust-lang.org/2022/07/19/Rust-1.62.1.html) in function coercions of `impl Trait` return types, and `async fn` lifetimes.
  We recommend that you update your Rust compiler to release 1.62.1, and re-compile Zebra.

### Added

- Add a `rayon` thread pool for CPU-bound tasks (#4776)
- Run multiple cryptographic batches concurrently within each verifier (#4776)
- Run deserialization of transaction in a rayon thread (#4801)
- Run CPU-intensive state updates in parallel rayon threads (#4802)
- Run CPU-intensive state reads in parallel rayon threads (#4805)
- Support Tiers and supported platforms per Tier doc (#4773)

### Changed

- Update column family names to match Zebra's database design (#4639)
- Update Zebra's mainnet and testnet checkpoints (#4777, #4833)
- Process more blocks and batch items concurrently, so there's a batch ready for each available CPU (#4776)
- Wrap note commitment trees in an `Arc`, to reduce CPU and memory usage (#4757)
- Increment `tokio` dependency from 1.19.2 to 1.20.0, to improve diagnostics (#4780)

### Fixed

- Only verify halo2 proofs once per transaction (#4752)
- Use a separate channel for each cryptographic batch, to avoid dropped batch results (#4750)
- Improve batch fairness and latency under heavy load (#4750, #4776)
- Run all verifier cryptography on blocking CPU-bound threads, using `tokio` and `rayon`  (#4750, #4776)
- Use `tokio`'s `PollSemaphore`, instead of an outdated `Semaphore` impl (#4750)
- Check batch worker tasks for panics and task termination (#4750, #4777)
- Limit the length of the `reject` network message's `message` and `reason` fields (#4687)
- Change the `bitvec` dependency from 1.0.0 to 1.0.1, to fix a performance regression (#4769)
- Fix an occasional panic when a `zebra-network` connection closes (#4782)
- Stop panicking when the connection error slot is not set (#4770)
- When writing blocks to disk, don't block other async tasks (#4199)
- When sending headers to peers, only deserialize the header data from disk (#4792)
- Return errors from `send_periodic_heartbeats_with_shutdown_handle` (#4756)
- Make FindHeaders and FindHashes run concurrently with state updates (#4826)
- Stop reading redundant blocks for every FindHashes and FindHeaders request (#4825)
- Generate sapling point outside the method (#4799)

#### CI

- Workaround lightwalletd hangs by waiting until we're near the tip (#4763)
- Split out Canopy logs into a separate job (#4730)
- Make full sync go all the way to the tip (#4709)
- Split Docker logs into sprout, other checkpoints, and full validation (#4704)
- Add a Zebra cached state update test, fix lightwalletd tests (#4813)


## [Zebra 1.0.0-beta.12](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.12) - 2022-06-29

This release improves Zebra's Orchard proof verification performance and sync performance.
Zebra prefers to connect to peers on the canonical Zcash ports.

This release also contains some breaking changes which:
- improve usability, and
- make Zebra compile faster.

### Breaking Changes

#### Cache Deletion

- Zebra deletes unused cached state directories in `<OS-specific cache dir>/zebra` (#4586)
  These caches only contain public chain data, so it is safe to delete them.

#### Compile-Time Features

- Most of Zebra's [tracing](https://github.com/ZcashFoundation/zebra/blob/main/book/src/user/tracing.md)
  and [metrics](https://github.com/ZcashFoundation/zebra/blob/main/book/src/user/metrics.md) features
  are off by default at compile time (#4539, #4680)
- The `enable-sentry` feature has been renamed to `sentry` (#4623)

#### Config

- Times in `zebrad.config` change from seconds/nanoseconds to a
  [human-readable format](https://docs.rs/humantime/latest/humantime/).
  Remove times in the old format, or use `zebrad generate` to create a new config. (#4587)

### Added

#### Diagnostics

- Show the current network upgrade in progress logs (#4694)
- Add some missing tracing spans (#4660)
- Add tokio-console support to zebrad (#4519, #4641)
- Add `fallible_impl_from` clippy lint (#4609)
- Add `unwrap_in_result` clippy lint (#4667)

#### Testing

- Check that old `zebrad.toml` configs can be parsed by the latest version (#4676)
- Test `cargo doc` warnings and errors (#4635, #4654)
- Document how to run full sync and lightwalletd tests (#4523)

#### Continuous Integration

- Add `beta` rust to CI (#4637, #4668)
- Build each Zebra crate individually (#4640)

### Changed

#### Chain Sync

- Update mainnet and testnet checkpoint hashes (#4708)

#### Diagnostics

- Update transaction verification dashboard to show all shielded pool sigs, proofs, nullifiers (#4585)

#### Testing

- Add an identifiable suffix to zcash-rpc-diff temp directories (#4577)

#### Dependencies

- Manage`cargo-mdbook` as a GitHub action (#4636)

#### Continuous Integration

- Automatically delete old GCP resources (#4598)

#### Documentation

- Improve the release checklist (#4568, #4595)

### Removed

#### Continuous Integration

- Remove redundant build-chain-no-features job (#4656)

### Fixed

#### Performance

- Upgrade `halo2` and related dependencies to improve proof verification speed (#4699)
- Change default sync config to improve reliability (#4662, #4670, #4679)
- Fix a lookahead config panic (#4662)

#### Continuous Integration

- Actually create a cached state image after running a sync (#4669)
- Split `docker run` into launch, `logs`, and `wait`, to avoid GitHub job timeouts (#4675, #4690)
- Ignore lightwalletd test hangs for now (#4663)
- Disable `zcash_rpc_conflict` test on macOS (#4614)
- Use `latest` lightwalletd image for Zebra's Dockerfile (#4599)
- Increase lightwalletd timeout, remove testnet tests (#4584)

#### Documentation

- Fix various `cargo doc` warnings (#4561, #4611, #4627)
- Clarify how Zebra and `zcashd` interact in `README.md` (#4570)
- Improve `lightwalletd` tutorial (#4566)
- Simplify README and link to detailed documentation (#4680)

#### Diagnostics

- Resolve some lifetime and reference lints  (#4578)

### Security

- When connecting to peers, ignore invalid ports, and prefer canonical ports (#4564)


## [Zebra 1.0.0-beta.11](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.11) - 2022-06-03

This release cleans up a lot of tech dept accumulated in the previous
development and improves the documentation.

### Added

- Log the tracing level when it is set or reloaded (#4515)

#### CI

- Add a codespell linting action (#4482)
- Add grpc tests to CI (#4453)
- Require network names in cached state disk names (#4392)

#### RPC

- Add support for `verbosity=1` in getblock (#4511)
- Add `z_gettreestate` gRPC tests (#4455)

#### Documentation

- Explain what Zebra does when it starts up (#4502)
- Add a lightwalletd tutorial (#4526)

### Changed

- Immediately disconnect from pre-NU5 nodes (#4538)
- Upgrade tracing-subscriber and related dependencies (#4517)
- Disable debug logging at compile time in release builds (#4516)
- Activate the mempool after 2 syncer runs at the chain tip, rather than 4 (#4501)
- Run coverage on stable (#4465)
- Allow more time for tests to end gracefully (#4469)
- Do not create draft PRs if not needed (#4540)

#### Rust Dependencies

- Bump inferno from 0.11.3 to 0.11.4 (#4534)
- Bump insta from 1.14.0 to 1.14.1 (#4542)
- Bump log from 0.4.14 to 0.4.17 (#4530)
- Bump serde_with from 1.13.0 to 1.14.0 (#4532)
- Bump indexmap from 1.8.1 to 1.8.2 (#4531)
- Bump vergen from 7.1.0 to 7.2.0 (#4521)
- Bump prost from 0.10.3 to 0.10.4 (#4490)
- Bump regex from 1.5.5 to 1.5.6 (#4463)
- Bump once_cell from 1.11.0 to 1.12.0 (#4462)
- Bump once_cell from 1.10.0 to 1.11.0 (#4447)


#### CI Dependencies

- Bump tj-actions/changed-files from 20 to 21 (#4510)
- Bump google-github-actions/auth from 0.7.3 to 0.8.0 (#4478)
- Bump tj-actions/changed-files from 21 to 22 (#4541)
- Bump w9jds/firebase-action from 2.1.0 to 2.1.2 (#4431)
- Bump reviewdog/action-actionlint from 1.24.0 to 1.25.0 (#4432)
- Bump reviewdog/action-actionlint from 1.25.0 to 1.25.1 (#4479)

### Fixed

- Index spending transaction IDs for each address (#4355)
- Resolve various clippy warnings (#4473)

#### Documentation

- Fix various doc warnings (#4514)
- Fix the syntax of links in comments (#4494)

#### CI

- Test RPCs with zcash/lightwalletd, to fix post-NU5 failures in adityapk00/lightwalletd (#4553)
- Add lightwalletd gRPC, clippy, rustfmt patch jobs (#4518)
- Permanently fix unreliable sync finished log regex (#4504)
- Always run patch jobs that depend on cached cloud disks (#4496)
- Temporarily finish full sync at 99% (#4457)
- Increase clippy timeout (#4472)
- Set a network env variable to be used in get-available-disks (#4477)
- Temporarily stop full sync at 97%, but send transactions at 100% (#4483)
- Mount the `lwd-cache` dir to the lightwalletd-full-sync (#4486)
- Require cached state for the send transactions test (#4487)
- Make reusable workflow job names match patch job names (#4466)
- Update docker patch jobs for recent changes (#4460)


## [Zebra 1.0.0-beta.10](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.10) - 2022-05-19

Zebra's latest beta continues adding support for `lightwalletd` RPC methods and continues with the testing of each of these features. Also, this beta sets the NU5 mainnet activation height.

### Added

#### RPC

- `z_gettreestate` RPC (#3990)

##### Tests

- grpc test for `GetTaddressBalanceStream` and `GetAddressUtxosStream` (#4407)
- snapshot tests for RPC methods responses (#4352 #4401)

#### CI

- `lightwalletd_update_sync` test to CI (#4269)
- `lightwalletd_full_sync` test to CI (#4268)

### Changed

- Set NU5 mainnet activation height and current network protocol version (#4390)
- NU5 mainnet dependency upgrades (#4405)
- Use the latest lightwalletd version (#4398)

#### Rust Dependencies

- orchard, redjubjub, jubjub, group, bls12_381, bitvec, halo2, jubjub, primitive_types,
  librustzcash, zcash_history, zcash_encoding, bellman, zcash_script, incrementalmerkletree (#4405)
- vergen from 7.0.0 to 7.1.0 (#4420)
- tokio-util from 0.7.1 to 0.7.2 (#4406)
- inferno from 0.11.2 to 0.11.3 (#4357)
- tokio from 1.18.1 to 1.18.2 (#4358)
- prost from 0.10.2 to 0.10.3 (#4348)
- bech32 from 0.8.1 to 0.9.0 (#4394)

#### CI Dependencies

- google-github-actions/auth from 0.7.1 to 0.7.3 (#4404 #4419)
- tj-actions/changed-files from 19 to 20 (#4403)
- w9jds/firebase-action from 2.0.0 to 2.1.0 (#4402)

#### Others

- Rename workflow files (#3941)
- Added block hash and height to syncer errors (#4287)
- Drop sentry dependencies when enable-sentry feature is disabled (#4372)
- Deprecate gcr.io as a registry and build faster (#4298)
- Clippy: Remove redundant bindings, allocations, and generics (#4353)

#### Documentation

- Added "old state directories aren't deleted" to known issues (#4365)
- Added support for Mermaid to render graphs (#4359)
- Fix some typos (#4397)

### Fixed

#### RPC

- Use the Sapling activation height in gRPC tests (#4424)

#### State

- Return non-finalized UTXOs and tx IDs in address queries (#4356)
- List cached state files before or after tests (#4409)

#### CI and testing fixes

- Updated Cargo.lock check job name in patch workflow (#4428)
- Put gRPC tests behind an optional feature flag to fix production build issues (#4369)
- Stop failing the send transaction test (#4416)
- Require cached lightwalletd state for the send transaction tests (#4303)
- Errors in Docker entrypoint (#4411)
- Only use cached state disks with the same state version (#4391)
- Output length in some tests (#4387)
- Wrong file being referenced by CI (#4364)
- Make test selection and logging consistent (#4375)
- Allow builds over 1 hour and tests without the sentry feature (#4370)


## [Zebra 1.0.0-beta.9](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.9) - 2022-05-06

Zebra's latest beta continues our work on `lightwalletd` RPC methods, and contains some internal CI improvements.

### Added

#### RPCs

- Add a script for comparing zcashd and zebrad RPC responses (#4219)
- Add Rust tests for lightwalletd sync from Zebra (#4177)
- Add integration test to send transactions using lightwalletd (#4068)
- RPC test with fully synced Zebra (#4157)
- Log unrecognized RPC requests (#3860)
- Implement the `get_address_tx_ids` RPC method query (#4119)
- Implement `getaddressbalance` RPC (#4138)
- Add a query function for transparent UTXOs (#4111)

#### CI

- Add `sending_transactions_using_lightwalletd` test to CI (#4267)
- Add a `zebrad tip-height` utility command (#4289)
- Add `fully_synced_rpc_test` test to CI (#4223)
- Add a reusable workflow for deployable integration tests (#4271)
- Add wallet grpc tests (#4253)
- Implement reusable workflows for image building (#4173)
- Implement `getaddressutxos` RPC method. (#4087)


### Changed

- Increase block validation timeouts (#4156)
- Decrease the peer handshake timeout to 3 seconds, to speed up the initial sync (#4212)
- Use link-time optimisation in release builds (#4184)
- Add an extra block retry, to speed up the initial sync (#4185)
- Update Zebra's block hash checkpoints (#4183)
- Document coinbase rules, refactor to ease understanding (#4056)
- Disconnect from testnet peers using the first NU5 testnet rules (#3976)

#### RPCs

- Simplify RPC types and add documentation (#4218)

#### Documentation

- Add transaction index diagram to RFC-0005 (#4330)

#### CI

- Skip tests when doing a manual full sync (#4333)
- Add cached state version to disk images (#4314)
- Check specific files for each job when linting (#4311)
- Use debian for faster mounting and bump readiness time (#4276)
- Use docker instead of Konlet for GCP deployments in CI (#4252)
- Create a full sync disk to add the cached state inside (#4266)
- Increase the Zcash parameter fetch timeout (#4148)

### Fixed

- Fix testnet syncer loop on large Orchard blocks (#4286)

#### RPCs

- Fix some RPC response formats to match `zcashd` (#4217)
- Make Zebra RPC compatible with the `zcash-cli` RPC client (#4215)
- Use a structure for parameters of getaddresstxids (#4264)

#### CI

- Only update cached states when needed (#4332)
- Run sync tests according to the right conditions (#4313)
- Stop actionlint from failing in main (#4317)
- Make the full sync tests cache state at `/zebrad-cache` (#4308)
- Avoid docker cache contamination and invalidation (#4254)
- Garbage collect instances no matter previous steps status (#4255)
- Do not delete instances from `main` branch on merge (#4206)
- Retry after docker log follow ssh failures (#4198)
- Share GitHub runner caches between branches (#4149)


## [Zebra 1.0.0-beta.8](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.8) - 2022-04-19

Zebra's latest beta completes our work on the NU5 consensus rules. It continues our work on `lightwalletd` RPC methods, and contains some internal CI improvements.

### Added

#### RPCs

- Implement a retry queue for transactions sent via RPCs (#4015)
- Partially implement the `getaddresstxids` RPC method (#4062)

#### State

- Add a transparent address balance index (#3963)
- Add a transparent address UTXO index (#3999)
- Add a transparent address transaction index (#4038)
- Add transparent address indexes to the non-finalized state (#4022)
- Add a query function for transparent address balances (#4097)

### CI

- Create cached state disk image after a successful full sync test (#3986)

### Changed

#### NU5

- Update to new zcash_script V5 API (#3799)
- Update network protocol versions for the second NU5 activation on testnet (#3799)
- Update the consensus branch ID and the second NU5 testnet activation height (#3799)
- Bump database version to trigger testnet rollback (#3799)

#### State

- Store transactions in a separate database index, to improve query speed (#3934)
- Store UTXOs by transaction location rather than transaction hash (#3978)
- Stop storing redundant transparent output fields in the database (#3992)
- Use LZ4 compression for RocksDB (#4027)
- Use Ribbon filters for database index lookups (#4040)
- Tune state database file compaction configuration (#4045)

#### CI

- Put the state version in cached state disk names (#4073)
- Improve mergify merge throughput (#4094, #4120)
- Run cached state rebuilds in main branch (#4107)
- Use GitHub Branch Protection checks instead of Mergify (#4103, #4105)
- Lint and standardize the actions structure (#3940)

#### Rust Dependencies

- Update shared Zcash dependencies for the second NU5 activation on testnet (#3799)
- Disable unused rocksdb compression features (#4082)
- Bump rlimit from 0.7.0 to 0.8.3 (#4051)

#### CI Dependencies

- Bump docker/metadata-action from 3.6.2 to 3.7.0 (#4049)
- Bump google-github-actions/auth from 0.6.0 to 0.7.0 (#4050)
- Bump tj-actions/changed-files from 18.6 to 18.7 (#4065)
- Bump reviewdog/action-actionlint from 1.21.0 to 1.23.0 (#4099, #4125)
- Bump actions/checkout from 3.0.0 to 3.0.1 (#4126)

### Fixed

#### CI

- Validate tests exit code after reading the container logs (#3968, #4069)
- Give enough time to zebra before reading logs (#4123)

#### Rust Clippy

- Ignore clippy drop warnings in tests (#4081)


## [Zebra 1.0.0-beta.7](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.7) - 2022-04-05

Zebra's latest beta fixes a `cargo install` build failure in the previous beta release. It also fixes a `lightwalletd` RPC bug, and improves test coverage.

### Changed

#### Database and State

- Update database design to put ordered list values in RocksDB keys (#3997)
- Make transparent address index database design more consistent (#4019)

#### CI

- Do not invalidate cache between PRs (#3996)

#### Dependency Updates

- Bump hyper from 0.14.17 to 0.14.18 (#3946)
- Bump indexmap from 1.8.0 to 1.8.1 (#4003)
- Bump semver from 1.0.6 to 1.0.7 (#3982)
- Bump serde-big-array from 0.3.2 to 0.4.1 (#4004)

##### Test Dependency Updates

- Bump insta from 1.13.0 to 1.14.0 (#3980)
- Bump tokio-util from 0.7.0 to 0.7.1 (#3981)

##### CI Dependency Updates

- Bump tj-actions/changed-files from 18.4 to 18.6 (#4002)

### Fixed

#### Build

- Fix a compilation error caused by a test-only method in production code (#4000)
- Add a job to ci.yml that does `cargo install --locked --path ./zebrad/ zebrad` (#3998)

#### RPC

- Tell `lightwalletd` to wait for missing blocks in the `getblock` RPC (#3977)

#### State

- Stop panicking when a state block commit fails (#4016)

#### Logging

- Log hashes as hex strings in block commitment errors (#4021)

#### Tests

- Check for accidental database incompatibilities in cached state tests (#4020)

## [Zebra 1.0.0-beta.6](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.6) - 2022-03-28

Zebra's latest beta adds RPC server support, including some of the RPC calls needed to become a **lightwalletd** back end.
As part of the RPC changes, we made performance improvements to cached state access.

### Added

#### RPC

- RPC server support (#3589 #3863)
- `getinfo` RPC method (#3660)
- `sendrawtransaction` RPC method (#3685 #3706)
- `getbestblockhash` RPC method (#3754 #3864)
- `getblock` RPC method (#3707)
- `getrawmempool` RPC method (#3851)
- `getblockchaininfo` RPC method (#3891)
- `getrawtransaction`RPC method (#3908)

#### Tests

- Basic RPC server tests (#3641 #3726 #3879)
- Lightwalletd integration test (#3619 #3627 #3628 #3859 #3824 #3758 #3903)
- Full sync test (#3582)

#### Documentation

- Document RPC methods (#3868)
- Document consensus rules from 4.6 Action Descriptions (#3549)

#### CI

- Add lightwaletd integration test build and CI pipeline (#3657 #3700 #3705)

#### Others

- Added `TransactionsByMinedId` to mempool (#3907)
- Added code owners and automatic review assignment to the repository (#3677 #3708 #3718)
- Validate ZIP-212 grace period blocks using checkpoints (#3889)
- Store Sapling and Orchard note commitment trees in finalized and non-finalized state (#3818)
- Get addresses from transparent outputs (#3802)
- Explain the different ways .txt files are usedin the CI (#3743)

### Changed

The Zebra team made a huge refactor to the database storage and the state to serve RPC calls efficiently. The refactor is accompanied with extensive low level tests in the form of snapshots.

#### Database and State

- Tests: #3691 #3759 #3630
- Database: #3717 #3741 #3874 #3578 #3579 #3590 #3607 #3617
- State: #3778 #3810 #3846 #3847 #3870 #3865 #3866 #3811 #3826

#### CI

- Cleanup GCP instances on a single PR (#3766)
- Use OCI Image Format Specification for labels (#3728)
- Use improved OIDC for gcloud authentication (#3885)
- Use gcloud to search for cached disk state (#3775)

#### Dependency updates

- Remove an outdated dependabot ignore rule (#3719)
- Manually upgraded some dependencies (#3625)
- Replace unmantained multiset with mset (#3595)
- Update sha2 from 0.9.8 to 0.9.9 (#3585)
- Update semver from 1.0.5 to 1.0.6 (#3610)
- Update secp256k1 from 0.21.2 to 0.21.3 (#3632)
- Update insta from 1.12.0 to 1.13.0 (#3762)
- Update inferno from 0.10.12 to 0.11.1 (#3748 #3919)
- Update regex from 1.5.4 to 1.5.5 (#3797)
- Update google-github-actions/setup-gcloud from 0.5.1 to 0.6.0 (#3814)
- Update docker/login-action from 1.12.0 to 1.14.1 (#3570 #3761)
- Update actions/cache from 2 to 3 (#3918)
- Update tj-actions/changed-files from 14.4 to 18.4 (#3667 #3917 #3796 #3895 #3876)
- Update docker/build-push-action from 2.9.0 to 2.10.0 (#3878)
- Update once_cell from 1.9.0 to 1.10.0 (#3747)
- Update actions/checkout from 2.4.0 to 3.0.0 (#3806)
- Update owo-colors from 3.2.0 to 3.3.0 (#3920)
- Update vergen from 6.0.2 to 7.0.0 (#3837)

#### Documentation

- Simplify the database design using prefix iterators (#3916)
- Link to Conventional Commits specification in CONTRIBUTING file (#3858)
- Update database design for read-only state service (#3843)
- Explain optional zebra-network/tor dependencies (#3765)
- Simplify zebra-checkpoints summary (#3612)

#### Tests

- Turn on full backtraces and disable frame filtering (#3763)
- Decouple full sync from other tests (#3735)
- Split zebrad acceptance tests into sub-modules (#3901)
- Improve zebrad test API (#3899 #3892)

#### Others

- Move mempool request and response types to a new `zebra-node-services` crate (#3648)
- Enable `checkpoint_sync` by default (#3777)
- Update Zebra's hard-coded blockchain checkpoint lists  (#3606)
- Clippy lints: warn on manual printing to stdout or stderr (#3767)

### Removed

- Temporally removed Windows support from CI (#3819)

### Fixed

- Make FromHex consistent with ToHex for tx/block hashes (#3893)
- Prevent synchronizer loop when very close to tip (#3854)
- Use RwLock for note commitment tree root caches (#3809)

#### Tests

- Use the correct stop condition for the cached state tests (#3688)
- Stop excessive logging which causes test hangs (#3755)
- Use all available checkpoints in the full sync test (#3613)
- Use `TEST_FAKE_ACTIVATION_HEIGHTS` at runtime and fix tests (#3749)
- Check for zebrad test output in the correct order (#3643)

#### CI

- Do not use GHA cache for images (#3794)
- Run coverage collection when pushing to main (#3561)
- Check for duplicate dependencies with optional features off (#3592)
- Remove coverage from mergify because nightly builds fail (#3886)
- Only run the full sync test on mergify queue PRs (#3773)
- Fix syntax in some yml workflows (#3957)
- Update CI job path triggers (#3692)
- Change pd-extreme to pd-ssd to avoid quotas (#3823)
- Use a specific shortening length for SHAs (#3929)
- Path format for cached state rebuild (#3873)
- Re-enable manual dispatch for test full sync (#3812)
- Use correct conditional in job to deploy Mainnet nodes (#3750)
- Missing job key and timeout bump for build (#3744)

### Security

- Forbid non-ascii identifiers (#3615)


## [Zebra 1.0.0-beta.5](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.5) - 2022-02-18

Zebra's latest beta brings better networking, documents more consensus rules and
improves the CI pipelines.

### Added

- Estimate network chain tip height based on local node time and current best tip (#3492)
- Add extra integer lints, and partially fix some code (#3409)
- Prepare for changes in ZIP-244 (#3415, #3446)
- Support large block heights (#3401)
- Check coinbase data is at least 2 bytes long (#3542)
- Ignore non-verack and non-version messages in handshake (#3522)
- Allow forcing zebrad to use color output (#3547)

#### Tests

- Add a test for peerset broadcast panic (#3470)
- Add PeerSet readiness and request future cancel-safety tests (#3252)
- Add full chain synchronization acceptance tests (#3543)
- Add chain tip estimate test: log chain progress while Zebra is syncing (#3495)

#### Networking

- Avoid repeated requests to peers after partial responses or errors (#3505)
- Send notfound when Zebra doesn't have a block or transaction (#3466)
- Route peer requests based on missing inventory (#3465)
- Create an API for a missing inventory registry, but don't register any missing inventory yet (#3255)

#### Documentation

- Document consensus rules from 7.3 Spend Description Encoding and Consensus (#3575)
- Document second part of consensus rules from 7.6 Block Header Encoding and Consensus (#3566)
- Document consensus rules from 3.9 Nullifier Sets (#3521)
- Update README goals and performance troubleshooting (#3525)
- Document consensus rules from 4.5 Output Descriptions (#3462)
- Document shielded pools consensus rules from 7.1.2 Transaction Consensus Rules (#3486)
- Document Transaction encodings: sprout fields (#3499)
- Document Transaction encodings: transparent fields (#3498)
- Document Transaction encodings: orchard fields (#3507)
- Document Transaction encodings: sapling fields (#3501)
- Document Transaction encodings: header fields (#3491)
- Document consensus rules from 4.4 Spend Descriptions (#3460)
- Document consensus rules from 4.3 JoinSplit Descriptions (#3452)
- Document Transaction consensus rules: Size rules (#3461)
- Document Transaction consensus rules: Coinbase rules (#3464)
- Document Transaction consensus rules: Header rules (#3456)

### Changed

- Reduce log level of components (#3418, #3437)
- Change Type To Force Consensus Rule Validation (#3544)
- Split The Database Module (#3568)
- Dockerize Tests And Run Sync In Detached Mode (#3459)
- Improve Docker And Gcloud Usage Without Cloud Build (#3431)
- Make better use of variables, secrets and versions (#3393)

### Removed

- Remove founders reward code (#3430)

### Fixed

- Use the new `increase_nofile_limit` function from `rlimit` 0.7.0 (#3539)
- Generate Well-Formed Finalsaplingroot In Arbitrary Implementation (#3573)
- Rename some lightwalletd database types (#3567)

#### Networking

- Allow more inbound than outbound connections (#3527)
- Only send responded updates on handshake/ping/pong (#3463)
- Increase state concurrency and syncer lookahead (#3455)
- Add a send timeout to outbound peer messages (#3417)

#### Tests

- Make Full Sync Test More Accurate (#3555)
- Create Disk From Image Before Mounting (#3550)
- Simplify Resource Conflict Test To Avoid Ci Failures (#3537)
- Make Full Sync Test More Efficient (#3562)
- Evaluate "if" conditions correctly and use last disk SHA (#3556)

#### CI

- Improve test requirements and merge conditions for Mergify (#3580)
- Make The Purpose Of Each Sync Test Clearer (#3574)
- Delete A Redundant "Test All" Job (#3552)
- Allow Branches With Dots In The Name (#3557)
- Allow Unprivileged Runs Of Clippy (#3558)
- New Lints In Nightly Rust (#3541)
- Typo In Paths Filtering Keyword (#3516)
- Do Not Wait For Deprecated Cloud Build (#3509)
- Restrict Merges With Unresolved Threads (#3453)
- Put PRs With No Priority Label In The Low Priority Queue (#3454)
- Temporarily allow forked repos to run PR workflows (#3503)

## [Zebra 1.0.0-beta.4](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.4) - 2022-01-26

Zebra's latest beta improves the networking code and fixes some bugs. A couple of fixed bugs had
caused Zebra to hang in some situations. Some improvements to the documentation were also included.
All Rust crates have now been updated to use Rust 2021 Edition.

### Added

- Add a copy-state zebrad command, which copies blocks between two state services (#3175)

#### Networking

- Add isolated Tor connection API, but don't enable it by default (#3303)
- Add a test for message broadcast to the right number of peers (#3284)

### Changed

- Update to use Rust edition 2021 (#3332)

#### Networking

- Cache incoming unsolicited address messages, and use them as responses (#3294)
- Cleanup internal network request handler, fix unused request logging (#3295)

#### Documentation

- Document the consensus rules for Spend Transfers, Output Transfers, and their Descriptions (Section 3.6 of the Zcash protocol specification) (#3338)
- Document the structure of the zebra-network crate (#3317)
- Document the consensus rules for Note Commitment Trees (Section 3.8 of the Zcash Protocol Specification) (#3319)
- Document chain value balances consensus rules with new format (#3286)
- Document part of the block header consensus rules (#3296)

### Fixed

#### Consensus

- Fix interstitial sprout anchors check (#3283)
- Check jubjub key correctness independent of redjubjub / jubjub (#3154)

#### Networking

- Fix some bugs related to isolated connections (#3302)
- Ignore unexpected block responses to fix error cascade when synchronizing blocks, improving synchronization speed (#3374)
- Cancel heartbeats that are waiting for a peer, rather than hanging Zebra (#3325)
- Stop ignoring some peers when updating the address book (#3292)
- Fix some address crawler timing issues (#3293)
- Retry Zcash sprout and sapling parameters download (#3306)
- Keep track of background peer tasks (#3253)

#### Chain Synchronization

- Fix deadlock in chain tip watch channel, that sometimes caused chain synchronization to hang (#3378)
- Fix syncer download order and add sync tests (#3168)

#### Tests

- Fix a type resolution error in the tests (#3304)

### Security

- Stop RocksDB and Tokio from calling unexpected code when zebrad exits (#3392)

## [Zebra 1.0.0-beta.3](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.3) - 2021-12-21

Zebra's latest beta works towards enforcing all consensus rules by validating JoinSplit Groth16 proofs
used by Sprout transactions. We have also added security and network improvements, and have also
added some metrics to help diagnose networking issues.

### Added

#### Consensus

- Validate JoinSplit proofs (#3128, #3180)

#### Networking

- Add and use `debug_skip_parameter_preload` config option in tests (#3197)
- Disconnect from outdated peers on network upgrade (#3108)

#### Metrics

- Add diagnostics for peer set hangs (#3203)
- Add debug-level Zebra network message tracing (#3170)

### Fixed

- Stop ignoring some connection errors that could make the peer set hang (#3200)
- Spawn initial handshakes in separate tasks, Credit: Equilibrium (#3189)
- Fix coinbase height deserialization to reject non-minimal encodings (#3129)
- Stop doing thousands of time checks each time we connect to a peer  (#3106)

### Security

- Stop ignoring panics in inbound handshakes (#3192)
- When there are no new peers, stop crawler using CPU and writing logs  (#3177)
- Limit address book size to limit memory usage (#3162)
- Drop blocks that are a long way ahead of the tip, or behind the finalized tip (#3167)


## [Zebra 1.0.0-beta.2](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.2) - 2021-12-03

Zebra's latest beta continues implementing zero-knowledge proof and note commitment tree validation. In this release, we have finished implementing transaction header, transaction amount, and Zebra-specific NU5 validation. (NU5 mainnet validation is waiting on an `orchard` crate update, and some consensus parameter updates.)

We also fix a number of security issues that could pose a local denial of service risk, or make it easier for an attacker to make a node follow a false chain.

As of this release, Zebra will automatically download and cache the Sprout and Sapling Groth16 circuit parameters. The cache uses around 1 GB of disk space. These cached parameters are shared across all Zebra and `zcashd` instances run by the same user.

### Added

#### Network Upgrade 5

- Validate orchard anchors (#3084)

#### Groth16 Circuit Parameters

- Automatically download and cache Zcash Sapling and Sprout parameters (#3057, #3085)
- Stop linking the Sapling parameters into the `zebrad` and Zebra test executables (#3057)

#### Proof & Anchor Verification

- Use prepared verifying key for non-batch Sapling Groth16 verification (#3092)
- Validate sapling anchors⚓ (#3084)
- Add Sprout anchors to `zebra-state` (#3100)

#### Transaction Amount & Header Validation

- Validate miner transaction fees (#3067, #3093)
- Validate transaction lock times (#3060)
- Validate transaction expiry height (#3082, #3103)

#### Dashboards

- Add transaction-verification.json Grafana dashboard (#3122)

### Fixed

- Shut down channels and tasks on PeerSet Drop (#3078)
- Re-order Zebra startup, so slow services are launched last (#3091)
- Fix slow Zebra startup times, to reduce CI failures (#3104)
- Speed up CI, and split unrelated and conflicting CI jobs (#3077)

### Security

- Stop closing connections on unexpected messages, Credit: Equilibrium (#3120, #3131)
- Stop routing inventory requests by peer address (#3090)

## [Zebra 1.0.0-beta.1](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.1) - 2021-11-19

Zebra's latest beta implements a number of consensus rules which will be needed for Zebra to fully validate all of the Zcash network consensus rules, including those which will activate with NU5.

With this release we are also fixing a number of security issues that could pose a DDoS risk or otherwise negatively impact other nodes on the network.

Finally, this release includes an upgrade to the latest version of tokio (1.14.0).

### Added

- Check Shielded Input and Output Limits (#3069, #3076)
- Sprout note commitment trees (#3051)
- Check per-block limits on transparent signature operations (#3049)
- Calculate Block Subsidy and Funding Streams (#3017, #3040)
- Check for duplicate crate dependencies in CI (#2986)
- Add unused seed peers to the Address Book (#2974, #3019)

#### Network Upgrade 5

- Verify Halo2 proofs as part of V5 transaction verification (#2645, #3039)
- ZIP-155: Parse `addrv2` in Zebra (#3008, #3014, #3020, #3021, #3022, #3032)
- ZIP 212: validate Sapling and Orchard output of coinbase transactions (#3029)
- Validate Orchard flags in v5 (#3035)

#### Documentation

- Mempool Documentation (#2978)

### Changed

- Upgrade cryptographic library dependencies (#3059)
- Upgrade to Tokio 1.14.0 (#2933, #2994, #3062)

#### Documentation

- README Updates (#2996, #3006)

### Fixed

- Stop downloading unnecessary blocks in Zebra acceptance tests (#3072)
- Implement graceful shutdown for the peer set (#3071)
- Check for panics in the address book updater task (#3064)
- Remove unused connection errors (#3054)
- Fix listener address conflicts in network tests (#3031)

### Security

- Security: Avoid reconnecting to peers that are likely unreachable (#3030)
- Security: Limit number of addresses sent to peers to avoid address book pollution (#3007)

## [Zebra 1.0.0-beta.0](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.0) - 2021-10-29

This is the first beta release of Zebra. Today the mempool work is fully finished and compatible with [ZIP-401](https://zips.z.cash/zip-0401) and several security issues in the network stack are fixed. In addition to that we improved our documentation specially in the `zebrad` crate while we increased our test coverage. Finally, we get started with the task of upgrading Tokio to version 1.

### Added

#### Mempool

- ZIP-401: weighted random mempool eviction (#2889, #2932)
- Reject a mempool transaction if it has internal spend conflicts (#2843)
- Limit transaction size in the mempool (#2917)

#### Cleanup and performance

- Remove unused mempool errors (#2941)
- Simplify calling add_initial_peers (#2945)
- Disable the new clippy::question_mark lint (#2946)
- Downgrade startup logs to debug to speed up CI (#2938)
- Speed up alternative state and zebrad tests in CI (#2929)

#### Tests

- Restore and update mempool tests (#2966)
- Test multiple chain resets (#2897)

### Security

- Track the number of active inbound and outbound peer connections so we can implement limits (#2912)
- Limit the number of initial peers (#2913)
- Rate-limit initial seed peer connections (#2943)
- Limit the number of inbound peer connections (#2961)
- Rate limit inbound connections (#2928)
- Limit the number of outbound peer connections (#2944)
- Reduce outgoing peers demand (#2969)

### Documentation

- Improve main `README` documentation and other book sections (#2967, #2894)
- Expand documentation for the mempool::crawler module (#2968)
- Improve mempool documentation (#2942, #2963, #2964, #2965)
- Improve documentation and types in the PeerSet (#2925)
- Update the documentation for value pools (#2919)
- Document why `CheckForVerifiedTransactions` is required for correctness (#2955)

#### Metrics

- Add user agent metrics (#2957)

### Changed

Part of the Tokio version 1 upgrade:

- Manually pin Sleep futures (#2914)
- Refactor handshake rate limiting to not store a Sleep type (#2915)
- Use single thread Tokio runtime for tests (#2916)

## [Zebra 1.0.0-alpha.19](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.19) - 2021-10-19

Zebra's latest alpha updates dependencies, improves metrics, gossips verified blocks and transactions, and continues the mempool-related implementation.

### Added

- Ignore `AlreadyInChain` error in the syncer (#2890)
- Give more information to the user in the wrong port init warning (#2853)
- Send looked up UTXOs to the transaction verifier (#2849)
- Return the transaction fee from the transaction verifier (#2876)
- Gossip recently verified block hashes to peers (#2729)
- Compute the serialized transaction size of unmined transactions (#2824)

#### Mempool

- Add a queue checker task, to make sure mempool transactions propagate (#2888)
- Store the transaction fee in the mempool storage (#2885)
- Add a debug config that enables the mempool (#2862)
- Pass the mempool config to the mempool (#2861)
- Add expired transactions to the mempool rejected list (#2852)
- Send AdvertiseTransactionIds to peers (#2823)
- Add a mempool config section (#2845)
- Add transactions that failed verification to the mempool rejected list (#2821)
- Un-reject mempool transactions if the rejection depends on the current tip
  (#2844)
- Split storage errors by match type: TXID or WTXID (#2833)
- Remove transactions in newly committed blocks from the mempool (#2827)

#### Documentation

- Improve the template for release versioning (#2906)
- Improve the documentation for the mempool transaction downloader (#2879)
- Add the documentation for the mempool storage and gossip modules (#2884)

#### Network Upgrade 5

- Set the minimum testnet network protocol version to NU5 (#2851)

#### Testing and CI

- Add some additional checks to the acceptance mempool test (#2880)

#### Metrics

- Refactor and add some new mempool metrics (#2878)
- Improve logging for initial peer connections (#2896)
- Always zero the mempool metrics when the mempool is disabled (#2875)
- Add a basic mempool storage Grafana dashboard (#2866)
- Add zcash.mempool.size.transactions and zcash.mempool.size.bytes metrics
  (#2860)
- Make some grafana labels shorter for graph readability (#2850)
- Make block metrics more accurate (#2835)

### Changed

#### Mempool

- Avoid broadcasting mempool rejected or expired transactions to peers (#2858)
- Encapsulate some mempool functions with the Mempool type (#2872)
- Remove duplicate IDs in mempool requests and responses (#2887)
- Refactor mempool spend conflict checks to increase performance (#2826)
- Rename mempool storage methods by match type (#2841)
- Remove unused mempool errors (#2831)

### Fixed

- Fix synchronization delay issue (#2921)
- Fix test failures by flushing output streams before exiting Zebra (#2911, #2923)
- Increase Zebra's restart acceptance test timeout (#2910)
- Avoid spurious acceptance test failures by decreasing the peer crawler timeout (#2905)
- Cancel pending download tasks when the mempool is disabled (#2886)
- Stop allowing some newly mined transactions into the mempool (#2874)
- Stop panicking when pruning unused queued blocks (#2842)

### Security

- Upgrade to ed25519-zebra 3.0.0 (#2864)
- Stop ignoring the mempool conflicting transaction reject list size limit (#2855)


## [Zebra 1.0.0-alpha.18](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.18) - 2021-10-05

Zebra's latest alpha updates dependencies, consensus parameters, and Orchard for NU5 testnet activation.
It continues our work on the mempool, including some mempool features that are used during network upgrade activation.

### Added

#### Mempool

- Send crawled transaction IDs to the mempool downloader (#2801)
- Cancel mempool download tasks when a network upgrade activates (#2816)
- Send mined transaction IDs to the mempool download/verify task for cancellation (#2786)
- Remove expired transactions from the mempool (#2774)
- Cancel download and verify tasks when the mempool is deactivated (#2764, #2754)
- Reject mempool transactions with conflicting spends (#2765)
- Clear mempool at a network upgrade (#2773, #2785)

#### Network Upgrade 5

- Update Zebra's advertised network protocol version to the latest NU5 testnet version (#2803)

#### Testing and CI

- Add tests to ensure mempool is working correctly (#2769, #2770, #2815)
- Create and use a helper MockService type to help with writing tests (#2810, #2748, #2790)
- Update Zebra tests to use the NU5 testnet activation height (#2802)
- Regenerate NU5 test cases with the latest network upgrade parameters (#2802)

#### Metrics

- Add Zebra metrics and a Grafana dashboard for connected peers and their network protocol versions (#2804, #2811)

### Changed

- Stop sending empty network messages to peers (#2791)
- Correctly validate transactions which never expire (#2782)

#### Network Upgrade 5

- Update `zcash_script` dependency to support V5 transactions (#2825)
- Set the NU5 testnet activation network upgrade parameters (#2802)
- Update shared Zcash Rust NU5 dependencies  (#2739)
- Update Zebra to use modified APIs in shared Zcash Rust NU5 dependencies (#2739)

### Fixed

- Stop panicking when using sync and async methods on the same `ChainTipChange` (#2800)
- Fix an incorrect assertion when the block locator is at the tip (#2789)
- Fix a missing NULL pointer check in `zebra_script`'s FFI (#2802)

### Security

#### Network Upgrade 5

- Update Zebra's orchard commitment calculations based on the latest orchard circuit (#2807)

## [Zebra 1.0.0-alpha.17](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.17) - 2021-09-14

Zebra's latest alpha continues work on the mempool.

### Added

- Monitor changes to the chain tip, decide if synchronization reached tip (#2695,
  #2685, #2686, #2715, #2721, #2722)
- Only enable the mempool crawler after synchronization reaches the chain tip (#2667)
- Reply to requests for transactions IDs in the mempool (#2720)
- Reply to requests for transactions in the mempool, given their IDs (#2725)
- Download and verify gossiped transactions (#2679, #2727, #2718, #2741)
- Internal additions and improvements to the mempool (#2742, #2747, #2749)

#### Documentation

- Document consensus rules for version group IDs (#2719)
- Specify Zebra Client will only support Unified Addresses (#2706)

### Fixed

-  Stop calculating transaction hashes twice in the checkpoint verifier (#2696)

### Security

- Replace older duplicate queued checkpoint blocks with the latest block's data (#2697)


## [Zebra 1.0.0-alpha.16](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.16) - 2021-08-27

Zebra's latest alpha finishes most of the state work needed for NU5 testnet activation and starts the mempool work.

### Added

#### Network Upgrade 5

- Finished ZIP-209: Prohibit Negative Shielded Chain Value Pool Balances (#2599, #2649, #2656, #2648, #2644)
- ZIP-221 and ZIP-244: Commitment validation in non-finalized state and checkpoint verifier (#2609, #2633)
- Orchard binding_verification_key support (#2441)

#### Mempool

- Mempool component, storage and errors (#2615, #2651)
- Create a transaction crawler for the mempool (#2646, #2672)
- Create and use types for unmined transactions and their IDs (#2634, #2666)
- ZIP-239: Implement mempool transaction v5 data structures and network messages (#2618, #2638)
- Return a transaction verifier from `zebra_consensus::init` (#2665)
- Provide recent syncer response lengths as a watch channel (#2602)
- Derive Copy and Clone for zebra-consensus errors (#2664)
- ChainTip trait creation and usage (#2677, #2676)

#### Metrics

- Add a network message to the grafana dashboard (#2673)

##### Documentation

- Refactor Zebra metrics docs (#2678, #2687)

### Fixed

- Stop converting `Message::Inv(TxId+)` into `Request::TransactionsById` (#2660)
- Fixed test.yml (#2635)
- Fixed a clippy lint (#2642)

## [Zebra 1.0.0-alpha.15](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.15) - 2021-08-16

Zebra's latest alpha contains the last of the changes to zebra chain state ahead of NU5 testnet activation and before concentrating on mempool work. The remaining NU5 validation will be completed prior to NU5 mainnet activation.

### Added

- Reject connections from outdated peers after network upgrade activation (#2519)

#### Network Upgrade 5

- ZIP-209: Prohibit Negative Shielded Chain Value Pool Balances Partial Implementation (#2546, #2554, #2561, #2569, #2576, #2577, #2578, #2596)
- ZIP-221: FlyClient - Consensus-Layer Changes Partial Implementation (#2531, #2553, #2583)
- ZIP-244: Implementation of authorizing data commitment (auth_digest) (#2547)
- Check remaining transaction value is non-negative (#2566)

### Changed

- Cache note commitment tree roots to speed up chain synchronisation  (#2584)
- Update state cache version to v9 (#2598)
- Refactor HistoryTree type into NonEmptyHistoryTree and HistoryTree types (#2582)

#### Testing and CI

- Reduce number of nullifier property test cases (#2574)
- Ensure `regenerate-stateful-test-disks.yml` syncs to the latest network upgrade (#2601)
- Increase coverage for generated chains and proptests (#2540, #2567)
- Remove unreliable tests for generated chains (#2548)
- Add test-only methods for modifying orchard shielded data and joinsplits (#2580)
- Generate test chains with valid chain value pools (#2597)
- Increase timeout of cached database creation (#2581)
- Use fixed genesis coinbase data in generated genesis blocks (#2568)
- Generate valid sapling shielded data for property tests (#2579)
- Optimize build to regenerate the test state faster (#2552)

### Fixed

- Fix the storage of anchors in the state (#2563)
- Make `zebra-state` compile successfully by itself (#2611)

#### Documentation

- Fix Transparent Value sign and definition (#2555)

## [Zebra 1.0.0-alpha.14](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.14) - 2021-07-29

Zebra's latest alpha continues our work on NU5, including Orchard and Transaction V5.

### Added

- Reject UTXO double spends (#2511)
- Add a ValueBalance type (#2505)
- Implement Sum for Amount (#2500)
- Add an OrderedUtxo type for transparent spend validation (#2502)
- Pass the finalized state to chain contextual validation (#2503)
- Calculate incremental note commitment trees (#2407)
- Reject duplicate Sapling and Orchard nullifiers (#2497)
- Add `proptest-impl` feature to `zebra-state` crate to help simplify tests(#2529)
- Track anchors and note commitment trees (#2458)
- Validate spends of transparent coinbase outputs (#2525)
- Calculate the remaining value in the transparent transaction value pool (#2486)

#### Documentation

- Add value pools design to book summary (#2520)

#### Testing

- Test consensus-critical Amount deserialization (#2487)
- Update to use new GitHub action names in Google Cloud workflows (#2533)
- Add test initialization helper function for tests (#2539)

### Changed

- Decrease the number of randomised test cases in zebra-state (#2521, #2513)
- Make nullifier tests consistent with UTXO tests (#2513)

#### Testing

 - Disable Rust beta tests in CI, due to a Rust bug (#2542)

### Fixed

- Clarify a variable name and check for overflow in new_ordered_outputs (#2510)
- Update Orchard keys, hashes, and note commitments to match Zcash test vectors (#2445)

## [Zebra 1.0.0-alpha.13](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.13) - 2021-07-15

Zebra's latest alpha continues our work on NU5, including Orchard and Transaction V5. New validation
rules were implemented for transactions, preventing double spends in the Sprout pool, checking
Sapling spends in V5 transactions, and some initial work to validate parts of Orchard transactions.

### Added

- Reject duplicate sprout nullifiers in the state (#2477)
- Add methods for getting block nullifiers (#2465)
- Verify orchard spend auth (#2442)
- Parse and ignore MSG_WTX inventory type in network messages (part of ZIP-239) (#2446)
- Add ZIP-244 signature hash support (#2165)
- Add HistoryTree struct (#2396)
- Add ZIP-0244 TxId Digest support (#2129)
- Validate V5 transactions with Sapling shielded data (#2437)

#### Documentation

- Document some consensus-critical finalized state behaviour (#2476)
- Value pools design (#2430)

### Changed

- Move Utxo type to zebra-chain (#2481)
- Combine near-duplicate Utxo creation functions (#2467)

#### Documentation

- Update state RFC for incremental trees, value pools, and RocksDB (#2456)
- Modify UTXO and state designs for transparent coinbase output checks (#2413)

### Fixed

- When a parent block is rejected, also reject its children (#2479)
- Restore the previous non-finalized chain if a block is invalid (#2478)
- Stop ignoring sapling binding signature errors (#2472)
- Always compute sighash with librustzcash (#2469)
- Fix bug in sighash calculation for coinbase transactions (#2459)
- Increase coverage of cached state tests: full block and transaction validation, and non-finalized
  state validation (#2463)

### Security

## [Zebra 1.0.0-alpha.12](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.12) - 2021-07-02

Zebra's latest alpha continues our work on NU5, including Orchard and Transaction V5. It also includes documentation updates and security fixes. In particular, Zebra no longer gossips unreachable addresses to other nodes and users should update and restart any running Zebra nodes.

### Added

- Check if a previous version of Zebra followed a different consensus branch (#2366)
- API for a minimum protocol version during initial block download (#2395)
- Add a verification height to mempool transaction verification requests (#2400)
- Improved error messages by adding database path method to the Finalized State (#2349)
- Add property test strategies for V5 transactions (#2347)
- Re-use a shared Tokio runtime for some Zebra tests (#2397)

#### Documentation

- Add CHANGELOG.md file to the zebra git repo (#2346)
- Client RFC updates (#2367, #2341)
- Explain how Zebra validates shielded coinbase outputs like other shielded outputs (#2382)
- Document required request timeouts due to data dependencies (#2337)
- Update known issues and add inbound network ports to the README (#2373)
- Document shared to per-spend anchor conversion (#2363)
- Document note commitment trees storage (#2259)

#### Network Upgrade 5

- Orchard note commitment tree test vectors (#2384)
- Enable V5 transaction test vectors in the groth16 tests (#2383)
- Validate transparent inputs and outputs in V5 transactions (#2302)
- Batch math & variable-time multiscalar multiplication for RedPallas (#2288)
- Implement asynchronous verifier service for RedPallas (#2318)
- ZIP-211: Validate Disabling Addition of New Value to the Sprout Value Pool (#2399)

### Changed

- Various transaction verifier refactors (#2371, #2432)
- Remove unicode in Zebra's user agent (#2376)
- Update multiple crates to ensure bitvec 0.22.3 is being used (#2351)
- Move transaction consensus branch ID check function to zebra-chain (#2354)
- Replace primitives_types with uint (#2350)
- Refactor to return errors from state update methods to prepare for contextual validation (#2417)
- Refactor of the validation of Sapling shielded data (#2419)

#### Network Upgrade 5

- Update spend and output checks for new Orchard consensus rules (#2398)

### Fixed

- Failed tests in the cached state CI workflow are no longer ignored (#2403)
- Stop skipping the cached sync tests in CI (#2402)
- Fix intermittent errors in the groth16 verifier tests (#2412)
- Skip IPv6 tests when SKIP_IPV6_TESTS environmental variable is set (#2405)
- Stop failing after the mandatory Canopy checkpoint due to incorrect coinbase script verification (#2404)
- Improved docs and panic messages for zebra_test::command (#2406)
- Gossip dynamic local listener ports to peers (#2277)
- Stop allowing JoinSplits for Halo (#2428)
- Fix failing legacy chain tests (#2427)

### Security

- Zebra no longer gossips unreachable addresses to other nodes (#2392)
  - User Action Required: Update and restart any running Zebra nodes
- Avoid duplicate peer connections (#2276)
- Send local listener address to peers (#2276)
- Limit reconnection rate to individual peers (#2275)

## [Zebra 1.0.0-alpha.11](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.11) - 2021-06-18

Zebra's latest alpha continues our work on NU5, including Orchard and Transaction V5. It also includes some security fixes.

### Added

- Add and use a function for the mandatory checkpoint for a given Network (#2314)

#### Network Upgrade 5

- ZIP-221: integrate MMR tree from librustcash (without Orchard) (#2227)

### Changed

- Replace usage of atomics with tokio::sync::watch in tests for CandidateSet (#2272)

#### Network Upgrade 5

- Use latest librustzcash version in zcash_history (#2332, #2345)

#### Testing and Diagnostics

- Refactor restart_stop_at_height test to make it more flexible (#2315)
- Use Swatinem/rust-cache@v1 (#2291)
- Replace bespoke source-based coverage config with cargo-llvm-cov (#2286)
- Remove outdated pinned nightly in coverage workflow (#2264)

### Fixed

#### Network Upgrade 5

- Stop panicking on invalid orchard nullifiers (#2267)
- Reject V5 transactions before NU5 activation (#2285)

#### Testing

- Make acceptance test zebrad output matching more robust (#2252)

### Security

- Stop gossiping failure and attempt times as last seen times (#2273)
- Return an error rather than panicking on invalid reserved orchard::Flags bits (#2284)
- Only apply the outbound connection rate-limit to actual connections (#2278)
- Rate limit initial genesis block download retries, Credit: Equilibrium (#2255)

## [Zebra 1.0.0-alpha.10](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.10) - 2021-06-09

Zebra's latest alpha continues our work on NU5, including Orchard and Transaction V5. It also includes some security fixes.

### Added

#### Network Upgrade 5

- Store Orchard nullifiers into the state (#2185)

#### Testing and Bug Reports

- Generate test chains that pass basic chain consistency tests (#2221)
- Make arbitrary block chains pass some genesis checks (#2208)
- Test Eq/PartialEq for Orchard keys (#2187, #2228)
- Further test new transaction consensus rules (#2246)
- Create workflow to regenerate cached state disks for tests (#2247)
- Add final sapling root test vectors (#2243)

### Changed

- Make sure the mandatory checkpoint includes the Canopy activation block (#2235)
- Move the check in transaction::check::sapling_balances_match to V4 deserialization (#2234)

#### Network Upgrade 5

- Implement more Transaction checks for Transaction Version 5 and Orchard (#2229, #2236)

#### Testing and Diagnostics

- Make debugging easier on proptests with large vectors (#2232, #2222)
- Update test job to use cached state version 5 (#2253)
- Add the database format to the panic metadata (#2249)

#### Developer Workflows

- Update the GitHub and RFC templates based on retrospectives (#2242)

### Fixed

- Get redpallas tweak proptests working again (#2219)

#### Testing

- Adjust the benchmark sample size so all benchmarks finish successfully (#2237)
- Fix scriptCode serialization and sighash test vectors (#2198)
- Allow multi-digit Zebra alpha versions in the zebrad acceptance tests (#2250)

### Security

- Don't trust future gossiped last seen times (#2178)
- Stop panicking when serializing out-of-range times, Credit: Equilibrium (#2203, #2210)
- Rate limit GetAddr messages to any peer, Credit: Equilibrium (#2254)
- Prevent bursts of reconnection attempts (#2251)

## [Zebra 1.0.0-alpha.9](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.9) - 2021-05-26

Zebra's latest alpha continues our work on NU5, including Orchard and Transaction V5, and includes several security fixes.

### Added
- Added a new `zcash_serialize_bytes` utility function (#2150)
- Added new Arbitrary impls for a number of types in zebra-chain and zebra-network (#2179)
- Zebra support for leap seconds (#2195)

#### Network Upgrade 5
- Zebra can now serialize and deserialize orchard shielded data (#2116)
- We now have some Action methods for orchard shielded data (#2199)

#### Testing and Bug Reports
- Added extra instrumentation for initialize and handshakes (#2122)

### Changed
- Collect and send more accurate peer addresses (#2123)
- Enable cargo env vars when there is no .git during a build, fix tag lookup, add build profile, add modified flag (#2065)

#### Testing
- Stop generating V1-V3 transactions for non-finalized state proptests (#2159)
- Added some logging to troubleshoot failing tests for redpallas signature (#2169)

### Fixed

- Fix clippy::cmp_owned for (sapling, orchard)::keys with ConstantTimeEq (#2184)

#### Documentation
- Fixed some typos and links in the documentation(#2157, #2174, #2180)

### Security
- Reject compact sizes greater than the protocol message limit (#2155)
- Handle small numbers of initial peers better (#2154)
  - This security issue was reported by Equilibrium
- Stop panicking on out-of-range version timestamps (#2148)
  - This security issue was reported by Equilibrium
- Stop gossiping temporary inbound remote addresses to peers (#2120)
  - If Zebra was configured with a valid (not unspecified) listener address, it would gossip the ephemeral ports of inbound connections to its peers. This fix stops Zebra sending these useless addresses to its mainnet and testnet peers.
- Avoid silently corrupting invalid times during serialization (#2149)
- Do version checks first, then send a verack response (#2121)
- Remove checkout credentials from GitHub actions (#2158)
- Make CandidateSet timeout and initial fanout more reliable (#2172)
- Remove CandidateSet state and add last seen time limit to validate_addrs (#2177)

## [Zebra 1.0.0-alpha.8](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.8) - 2021-05-12

Zebra's latest alpha continues our work on NU5, including Orchard and Transaction V5.

### Added

#### Network Upgrade 5

- Continue implementation of Transaction V5 (#2070, #2075, #2100)
- Implementation of data structures for Orchard support in Zebra (#1885)
- Implementation of redpallas in Zebra (#2099)

#### Testing and Bug Reports

- Enable more Transaction v5 tests (#2063)

#### Documentation

- Document how Zebra does cross-crate proptests (#2069, #2071)
- Explain how to derive arbitrary impls in the dev docs (#2081)
- Async in Zebra RFC (#1965, #2111)
- Fixes and updates to Zebra Book TOC and process (#2124, #2126)
- Improvements to release process (#2138)
- Explicitly allow unencrypted disclosures for alpha releases (#2127)

### Changed

#### Refactors and Cleanups

- Clippy nightly: disable owned cmp, stop comparing bool using assert_eq (#2073, #2117)

### Fixed

#### Testing and Logging

- Remove broken ci-success job, which was skipping some required checks (#2084)
- Improve CI speed by removing redundant build jobs and Rust components (#2088)
- Fix a bad merge that was committed to main (#2085)

## [Zebra v1.0.0-alpha.7](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.7) - 2021-04-23

Zebra's latest alpha continues our work on NU5/Orchard, and fixes some security and protocol correctness issues.

Zebra now has best-effort support for Apple M1 builds, and logging to systemd-journald.

### Added

#### Network Upgrade 5

- Implement Sapling serialization in Transaction V5 (#1996, #2017, #2020, #2021, #2057)
- Draft RFC: Treestate management (#983)

#### Configuration and Logging

- Stop requiring a port for Zcash listener addresses (#2043)
  - use the default port if there is no configured port
- Add journald support through tracing-journald (#2034)
  - Zebra does not have any journald integration tests, so we will support it on a best-effort basis

#### Testing and Bug Reports

- Benchmark Block struct serialization code (#2018)
- Add the new commit count and git hash to the version in bug reports (#2038)
- Add branch, commit time, and build target to the panic metadata (#2028)
- Automatically update app version from crate version (#2028)

### Changed

#### Supported Platforms and Dependencies

- Update dependencies to support Apple M1 (#2026)
  - Zebra does not have any Apple M1 CI, so we will support it on a best-effort basis
- Bump ripemd160 from 0.8.0 to 0.9.1 and remove trait import (#2027)
- Update to vergen 5 (#2029)

#### Refactors and Cleanups

- Refactor and document correctness for std::sync::Mutex in ErrorSlot (#2032)
- Refactor and document correctness for std::sync::Mutex<AddressBook> (#2033)
- Make Zcash string serialization consistent with deserialization (#2053)
- clippy: make to_* methods take self by value (#2006)

#### Testing

- Speedup proptests for Chain struct in zebra-state (#2012)

### Fixed

- Stop assuming there will always be a `.git` directory during builds (#2037)
- Clarify CandidateSet state diagram (#2036)

#### Network Protocol

- Stop panicking when Zebra sends a reject without extra data (#2016)
- Switch to an async mutex for handshake nonces (#2031)

#### Testing and Logging

- Fix a test failure due to ' debug format changes in Rust (#2014)
- Fix Windows CI LLVM paths (#2026)
- Clarify a duplicate log message (#2054)

### Security

#### Network

- Avoid a single peer providing a majority of Zebra's peer addresses (#2004)
- Make sure handshake version negotiation always has a timeout (#2008)
- Make sure each peer heartbeat has a timeout (#2009)

#### Memory Usage

- Implement vector deserialisation limits for new Transaction::V5 types (#1996)



## [Zebra v1.0.0-alpha.6](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.6) - 2021-04-09

Zebra's latest alpha includes more validation of pre-NU5 consensus rules, continues our work on NU5/Orchard, and fixes some security and protocol correctness issues.

The Zebra project now has a [Code of Conduct](https://github.com/ZcashFoundation/zebra/blob/main/CODE_OF_CONDUCT.md).

### Added

- Design for Transaction V5 (#1886)
  - Make shielded data and spends generic over Transaction V4 and V5 (#1946, #1989)
- Async batching for:
  - Sprout `JoinSplit` signatures (#1952)
  - Sapling `Spend` and `Output` Groth16 proofs (#1713)
- Enable `Joinsplit` and `Spend` spend auth sighash verification (#1940)
- Randomised property tests for `InventoryHash` and `MetaAddr` (#1985)

#### Documentation

- Update the RFC process to include draft RFCs (#1962)
  - Merge some open RFCs as drafts (#1006, #1007, #1063, #1129)
- Add a fast start option to the Zebra Client RFC (#1969)
- Document that Zebra's mandatory checkpoint can change (#1935)

### Changed
- Refactor the Block Commitment field based on ZIP-244 (#1957, #1978, #1988)

### Fixed
- Stop ignoring inbound message errors and handshake timeouts (#1950)
- Don't send a useless heartbeat when the peer connection is already closing (#1950)

### Security

- Reduce deserialized memory usage for malicious blocks (#1920, #1977)
- Ensure that new MetaAddr fields are sanitized (#1942)
- Fix a deadlock between the peer address crawler and peer dialer when all connections fail (#1950)
- Avoid starvation of handshakes and crawling under heavy syncer load (#1950)
- Avoid starvation of request cancellations and timeouts under heavy peer response load (#1950)
- Async network code correctness documentation (#1954, #1972)

## [Zebra v1.0.0-alpha.5](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.5) - 2021-03-23

Zebra's latest alpha checkpoints on Canopy activation, continues our work on NU5, and fixes a security issue.

Some notable changes include:

### Added
- Log address book metrics when PeerSet or CandidateSet don't have many peers (#1906)
- Document test coverage workflow (#1919)
- Add a final job to CI, so we can easily require all the CI jobs to pass (#1927)

### Changed
- Zebra has moved its mandatory checkpoint from Sapling to Canopy (#1898, #1926)
  - This is a breaking change for users that depend on the exact height of the mandatory checkpoint.

### Fixed
- tower-batch: wake waiting workers on close to avoid hangs (#1908)
- Assert that pre-Canopy blocks use checkpointing (#1909)
- Fix CI disk space usage by disabling incremental compilation in coverage builds (#1923)

### Security
- Stop relying on unchecked length fields when preallocating vectors (#1925)

## [Zebra v1.0.0-alpha.4](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.4) - 2021-03-17

Zebra's latest alpha starts our work on NU5 and fixes several security issues.

Some notable changes include:

### Added

#### Network Upgrade 5

- Add some NU5 constants to zebra (#1823)
- Start work on transaction version 5 (#1824)

#### Metrics

- Add Grafana dashboards (#1830)
- Add Zebra version info to metrics (#1900)
- Add message type tag to message metrics (#1900)

### Changed

- [ZIP-215 Explicitly Defining and Modifying Ed25519 Validation Rules](https://github.com/zcash/zips/blob/master/zip-0215.rst) (#1811)
- Metrics renaming to enable node interoperability (#1900)
  - Renaming metrics breaks existing Grafana configs
- Rename config network.new_peer_interval to crawl_new_peer_interval (#1855)
  - This change includes a backwards-compatibility alias, so existing configs do not need to be updated

### Fixed

#### Code Style

- Apply nightly clippy suggestions (#1834)

#### Documentation

- Re-enable [zebra.zfnd.org](https://zebra.zfnd.org/) deployment (#1792)
- Document and log trailing message bytes (#1888)
- Move design/data-flow to rfcs/drafts (#1825)
- Document how inbound connections are added to the CandidateSet (#1852)

#### Hangs and Panics

- Fix a peer DNS resolution edge case (#1796)
- Stop sending blocks and transactions on the first error (#1818)
- Revert a connection refactor that caused frequent hangs (#1803)

#### Testing

- Avoid acceptance test port conflicts (#1812)
- Re-enable the checkpoint verifier restart tests (#1837)
- Adjust the crawl interval and acceptance test timeout (#1878)
- Explicitly auto-delete additional cache disks (#1859)

### Security

- Reduce inbound concurrency to limit memory usage (#1881)
- Verify proof-of-work in the checkpoint verifier (#1882)
- Implement outbound connection rate limiting (#1855)
- Document that the configured Zcash listener IP address is advertised to remote peers (#1891)

## [Zebra v1.0.0-alpha.3](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.3) - 2021-02-23

Zebra's latest alpha brings multiple reliability and stability improvements for node startup, long-running syncs, and testing.

Some notable changes include:

### Added
- Add beta rust to CI (#1725)
- Add Usability Testing Plan GitHub issue template (#1519)
- Add Release Checklist GitHub pull request template (#1717)

### Changed
- Compute the network message body length to reduce heap allocations (#1773)
- Re-enable the macOS conflict acceptance tests (#1778)
- Re-enable coverage CI (#1758, #1787)
- Disable fail-fast in the CI test job (#1776)
- Rename responsible_disclosure.md to SECURITY.md (#1747)

### Removed
- Disable unreliable testnet large sync test (#1789)

### Fixed

#### Hangs and Panics
- Refactor `connection.rs` to make `fail_with` panics impossible (#1721)
- Stop ignoring failed peer addresses (#1709)
- Retry initial peer DNS resolution on failure (#1762)
- Update tower-batch semaphore implementation (#1764)
- Use ready! in ChainVerifier::poll_ready (#1735)
- Use CallAllUnordered in peer_set::add_initial_peers (#1734)

#### Testing
- Bump CI build and test timeouts to 60 minutes (#1757)
- Run CI workflow on push to main & manual request (#1748)
- Set SKIP_NETWORK_TESTS using Windows syntax (#1782)
- Fix Windows build failures due to disk space (#1726)
- Fix acceptance test timeouts, cleanup, and diagnostics (#1736, #1766, #1770, #1777)

#### Logging and Metrics
- Update PeerSet metrics after every change (#1727)
- Log initial peer connection failures (#1763)

## [Zebra v1.0.0-alpha.2](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.2) - 2021-02-09

Zebra's latest alpha brings multiple reliability and stability improvements for node startup, node shutdown, and long-running syncs.

Some notable changes include:

### Added
- Asynchronous Groth16 verification (#830)
- Security disclosure principles (#1650)

### Changed
- Document that connect\_isolated only works on mainnet (#1693)
- Document the impact of the redjubjub channel bound (#1682)
- Log when the syncer awaits peer readiness (#1714)

### Fixed
- Fix shutdown panics (#1637)
- Add hints to port conflict and lock file panics (#1535)
- Perform DNS seeder lookups concurrently, and add timeouts (#1662)
- Avoid buffer slot leaks in the Inbound service (#1620)
- Avoid a buffer slot leak by removing CallAllUnordered (#1705)
- Avoid future buffer slot leaks in ChainVerifier (#1700)
- Limit inbound download and verify queue (#1622)
- Increase a tower-batch queue bound (#1691)
- Fix a f64::NAN metrics sentinel (#1642)
- Actually use `VerifyCheckpointError::CommitFinalized` (#1706)

## [Zebra v1.0.0-alpha.1](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.1) - 2021-01-30

Zebra's second alpha brings multiple reliability and stability improvements for long-running syncs.

We've resolved known panics during syncing, and reduced the number of sync hangs.

Some notable changes include:

### Added
- Add peer set tracing (#1468)
- Add Sentry support behind a feature flag (#1461)
- Log configured network in every log message (#1568)

### Changed
- Export new precompute api in zebra-script (#1493)
- Rewrite peer block request handler to match the zcashd implementation (#1518)

### Fixed
- Avoid panics when there are multiple failures on the same connection (#1600)
- Add sync and inbound timeouts to prevent hangs (#1586)
- Fix Zebra versions so all crates are on the 1.0.0-alpha series (#1488)
- Make 'cargo run' use 'zebrad' rather than failing (#1569)
- Panic if the lookahead limit is misconfigured (#1589)
- Recommend using --locked with 'cargo install' (#1490)
- Simplify C++ compiler dependency in the README (#1498)
- Stop failing acceptance tests if their directories already exist (#1588)
- Stop panicking when ClientRequests return an error (#1531)
- Upgrade to tokio 0.3.6 to avoid a time wheel panic (#1583, #1511)

Currently, Zebra does not validate all the Zcash consensus rules.

## [Zebra v1.0.0-alpha.0](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.0) - 2021-01-30

Zebra first alpha release 🎉

The goals of this release are to:
- participate in the Zcash network,
- replicate the Zcash chain state,
- implement the Zcash proof of work consensus rules, and
- sync on Mainnet under excellent network conditions.

Currently, Zebra does not validate all the Zcash consensus rules.
It may be unreliable on Testnet, and under less-than-perfect
network conditions.
### Added

- zebrad: Optional HTTP health endpoints for cloud-native readiness and liveness checks.
  When configured, zebrad serves two simple HTTP/1.1 endpoints on a dedicated listener:
  - GET /healthy: returns 200 when the process is up and has at least the configured number of recently live peers; otherwise 503.
  - GET /ready: returns 200 when the node is near the chain tip and the estimated block lag is within the configured threshold; otherwise 503.
  Configure via the new [health] section in zebrad.toml:
  - health.listen_addr (optional, enables the server when set)
  - health.min_connected_peers (default 1)
  - health.ready_max_blocks_behind (default 2)
  - health.enforce_on_test_networks (default false)
  See the Zebra Book for examples and Kubernetes probes: https://zebra.zfnd.org/user/health.html

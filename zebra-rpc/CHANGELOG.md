# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Removed

- `methods::types::get_block_template::fetch_chain_info`

## [3.1.0] - 2025-11-17

## Added

- Populated `asm` field returned by Zebra's RPC methods with code in script outputs as well as script types ([#10019](https://github.com/ZcashFoundation/zebra/pull/10019))

### Fixed

- Republicized `valid_addresses` method ([#10021](https://github.com/ZcashFoundation/zebra/pull/10021))

## [3.0.0] - 2025-10-15

In this release we continue refining the RPC interface as part of the zcashd deprecation
process and third-party integration improvements.

### Breaking Changes

- Removed the `GetAddressBalanceRequest::valid_address_strings` method.
- Changed `GetTreestateResponse::new()` to take six parameters instead of five.
- Changed `Commitments::new()` to take the new `final_root` parameter.
- Changed `TransactionObject::new()` to take 26 parameters instead of 25.
- Changed `Orchard::new()` to take seven parameters instead of three.
- Marked `GetTreestateResponse::{from_parts, into_parts}` as deprecated.
- The `RpcServer` trait is no longer sealed, allowing external implementations.

### Changed

- Allow `zebra-rpc` to be compiled without `protoc` ([#9819](https://github.com/ZcashFoundation/zebra/pull/9819))

### Added

- `getmempoolinfo` RPC method ([#9870](https://github.com/ZcashFoundation/zebra/pull/9870))
- `getnetworkinfo` RPC method ([#9887](https://github.com/ZcashFoundation/zebra/pull/9887))
- Support for the `chainInfo` field in `getaddressutxos` RPC method ([#9875](https://github.com/ZcashFoundation/zebra/pull/9875))
- Introduce `BytesInDisplayOrder` trait to standardize byte-reversed encoding in RPC ([#9810](https://github.com/ZcashFoundation/zebra/pull/9810))

### Fixed

- Use `STANDARD` Base64 for RPC auth encoding/decoding ([#9968](https://github.com/ZcashFoundation/zebra/pull/9968))
- Fixed issue around copying generated files to output directory when `protoc` or `.proto` files are unavailable ([#10006](https://github.com/ZcashFoundation/zebra/pull/10006))


## [2.0.1] - 2025-08-22

### Changed

- Removed dependency on `protoc`

## [2.0.0] - 2025-08-07

### Breaking Changes

- Changed the `deferred` value pool identifier to `lockbox` in `getblock` and
  `getblockchaininfo`.

### Changed

- Slice `[GetBlockchainInfoBalance; 5]` type is aliased as `BlockchainValuePoolBalances`

## [1.0.0] - 2025-07-11

First "stable" release. However, be advised that the API may still greatly
change so major version bumps can be common.

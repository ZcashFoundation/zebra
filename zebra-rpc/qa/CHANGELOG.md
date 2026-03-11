# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

We do not publish package releases or Git tags for this project. Each version entry in this changelog represents the state of the project at the commit where the changelog update was merged.

## [Unreleased]

## [0.1.0] - 2025-11-11

In this first (pseudo)-release, we document all the changes made while migrating from the original Zcash test framework (`zcashd` only) to the new Z3 stack (`zebrad` + `zallet`), now hosted within the Zebra repository as part of the zebra-rpc crate.

As a reference, we cloned the Zcash repository at the `v6.10.0` tag and replaced its entire `qa` folder with the one developed for Zebra.
The resulting diff can be viewed here: https://github.com/zcash/zcash/compare/v6.10.0...oxarbitrage:zcash:zebra-qa?expand=1

Most of the legacy tests were removed. Since all tests require substantial modification to work with the new stack, we decided to migrate the framework first and then port tests individually as needed.

Below is a summary of the major changes:

### Deleted

- All existing tests were removed.
- `util.py`: removed Zcash-specific functionality.
- Compressed data folders (`cache`, `golden`) were deleted.
- The zcash folder (which contained smoke tests, dependency tests, and the full test suite) was removed.

### Changed

- Adapted existing `zcashd` utilities for use with `zebrad`.
- Disabled cache functionality (the supporting code remains available for future re-enablement).
- Updated argument handling for the Zebra node (different from `zcashd`).
- The following tests were modified to run under the new framework:
    - `wallet.py`
    - `nuparams.py`
    - `getmininginfo.py`
    - `feature_nu6_1.py`
    - `create_cache.py`

### Added

- New test cases:
    - `addnode.py`
    - `getrawtransaction_sidechain.py`
    - `fix_block_commitments.py`
    - `feature_nu6.py`
    - `feature_backup_non_finalized_state.py`
- Introduced a non-authenticated proxy (`proxy.py`), cloned from `authproxy.py` but with authentication removed. 
- Integrated the `zallet` process into the framework. Since the new stack requires both node and wallet processes, utilities for starting, configuring, and managing `zallet` were added following the same pattern as the node helpers.
- Added a `toml` dependency for manipulating Zebra and Zallet configuration files.
- Introduced base configuration templates for both Zebra and Zallet. The framework updates these at startup (setting ports and other runtime data), while tests can override parameters as needed.
- Added a configuration module to simplify passing runtime arguments from tests to the Zebra node.

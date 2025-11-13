# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [3.0.1] - 2025-11-13

### Added

- Added `BlockVerifierService` helper trait for convenience when constraining generic type parameters ([#10010](https://github.com/ZcashFoundation/zebra/pull/10010))

## [3.0.0] - 2025-10-15

In this release, the Sapling parameters were refactored ([#9678](https://github.com/ZcashFoundation/zebra/pull/9678)),
and the Sapling verifiers now use the `BatchValidator`. The transaction verifier was also updated to use
`orchard::bundle::BatchValidator` ([#9308](https://github.com/ZcashFoundation/zebra/pull/9308)).

Additionally, the checkpoint logic was moved out of `zebra-consensus` into `zebra-chain`
to simplify configuration in testnet and regtest modes. ([#9888](https://github.com/ZcashFoundation/zebra/pull/9888))

### Breaking Changes

- Removed public statics used for Groth16 verification:
    - `SPEND_VERIFIER`
    - `OUTPUT_VERIFIER`
- Removed or renamed public structs:
    - `zebra_consensus::groth16::Groth16Parameters`
    - `zebra_consensus::groth16::GROTH16_PARAMETERS`
    - `zebra_consensus::CheckpointList`
    - `zebra_consensus::halo2::BatchVerifier`
- Removed public trait:
    - `zebra_consensus::ParameterCheckpoint`
- Moved checkpoint configuration and related structures to zebra-chain.


## [2.0.0] - 2025-08-07

Support for NU6.1 testnet activation.

### Breaking Changes

- Renamed `legacy_sigop_count` to `sigops` in `BlockError::TooManyTransparentSignatureOperations` and `transaction::Response::Block`.


## [1.0.0] - 2025-07-11

First "stable" release. However, be advised that the API may still greatly
change so major version bumps can be common.

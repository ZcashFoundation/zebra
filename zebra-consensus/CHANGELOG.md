# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [2.0.0] - 2025-08-07

Support for NU6.1 testnet activation.

### Breaking Changes

- Renamed `legacy_sigop_count` to `sigops` in
  `BlockError::TooManyTransparentSignatureOperations` and `Response::Block`.


## [1.0.0] - 2025-07-11

First "stable" release. However, be advised that the API may still greatly
change so major version bumps can be common.

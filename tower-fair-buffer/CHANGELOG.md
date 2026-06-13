# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - Unreleased

Initial release. Be advised that the API may still greatly change so major
version bumps can be common.

### Added

- `FairBuffer`, a fork of tower 0.4.13's `tower::buffer` that queues requests
  in a `crossbeam_skiplist::SkipMap` ordered by each caller's recent request
  count, serves the quietest caller first, and sheds the loudest caller's
  queued request when full
  ([#7306](https://github.com/ZcashFoundation/zebra/issues/7306))

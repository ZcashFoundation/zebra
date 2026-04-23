# Zebrad Test Architecture

Tests are organized into a single binary (`zebrad-tests`) with three module tiers.

## Structure

```shell
zebrad/tests/
  main.rs               # Entry point (mod common, unit, integration, stateful)
  common/               # Shared test helpers (not test functions)
  unit/                 # Fast tests: CLI, config, end-of-support (<1 min)
  integration/          # Tests that launch zebrad, no cached state (5-15 min)
    sync.rs             # Checkpoint sync, restart
    rpc.rs              # RPC/metrics/tracing endpoints
    database.rs         # State format, conflicts, migrations
    regtest.rs          # Regtest mode, chain sync, funding streams
    mempool.rs          # Mempool activation
    network.rs          # Port conflicts, lightwalletd integration, peer behavior
  stateful/             # Tests requiring cached blockchain state (30 min - days)
    sync.rs             # Full sync, mandatory checkpoint sync
    rpc.rs              # Block template, submit block, snapshot tests
    lightwalletd.rs     # LWD gRPC tests (feature-gated)
    checkpoints.rs      # Checkpoint generation (feature-gated)
    indexer.rs           # Indexer tests (feature-gated)
```

## Adding a new test

1. Put the test function in the appropriate module file
2. Use `#[ignore]` on stateful tests as a `cargo test` safety net
3. No nextest configuration changes needed (module paths handle filtering)

## Running tests

```bash
# Default: unit + integration tests (excludes stateful)
cargo nextest run

# Specific category
cargo nextest run -E 'test(/^unit::/)'
cargo nextest run -E 'test(/^integration::/)'
cargo nextest run -E 'test(/^stateful::/)'

# Specific test
cargo nextest run -E 'test(sync_one_checkpoint_mainnet)'

# CI profiles
cargo nextest run --profile ci              # PR tests (unit + integration)
cargo nextest run --profile ci-stateful \
  -E 'test(sync_full_mainnet)'             # GCP stateful test
```

## Nextest profiles

| Profile | Scope | Used by |
| --------- | ------- | --------- |
| `default` | unit + integration | Local dev |
| `ci` | unit + integration | PR CI (tests-unit.yml) |
| `ci-stateful` | stateful (via --filter-expr) | GCP CI |
| `check-no-git-dependencies` | Single release check | Release PRs |

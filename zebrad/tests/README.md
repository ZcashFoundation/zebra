# Zebrad Test Architecture

Tests are organized into a single binary (`zebrad-tests`) with module-based
tiers. The canonical tier definitions and local `cargo nextest` examples live in
[`main.rs`](main.rs). Nextest profile filters and timeouts live in
[`../../.config/nextest.toml`](../../.config/nextest.toml).

## Structure

```shell
zebrad/tests/
  main.rs               # Entry point (mod common, unit, integration, stateful, e2e)
  common/               # Shared test helpers (not test functions)
  unit/                 # Fast local tests
  integration/          # Zebrad process, regtest, RPC, database, and bounded sync tests
  stateful/             # Cached-state and runtime lightwalletd tests
  e2e/                  # Full-system public-network tests
```

## Adding a new test

1. Put the test function in the appropriate module file
2. Use `#[ignore]` on stateful and E2E tests as a `cargo test` safety net
3. No nextest configuration changes needed (module paths handle filtering)

## Nextest profiles

| Profile | Scope | Used by |
| --------- | ------- | --------- |
| `default` | unit + integration | Local dev |
| `ci` | unit + integration | PR CI (tests-unit.yml) |
| `ci-stateful` | stateful (via --filter-expr) | GCP CI |
| `ci-e2e` | E2E (via --filter-expr) | GCP CI |
| `check-no-git-dependencies` | Single release check | Release PRs |

---
status: accepted
date: 2026-04-08
story: Refactor test architecture for scalability, discoverability, and simplified CI configuration. https://github.com/ZcashFoundation/zebra/issues/10136
---

# Module-Based Test Architecture with Structural Categorization

## Context & Problem Statement

Zebra's integration tests lived in a single 4,440-line file (`zebrad/tests/acceptance.rs`) containing 68 test functions that spanned CLI smoke tests, RPC endpoint tests, full chain sync tests, lightwalletd gRPC tests, regtest mode tests, checkpoint generation, and database migration tests. This monolith was supported by a nextest configuration with 23 profiles — almost all targeting a single test function by exact name — plus a manually maintained exclusion denylist of 14 test names in the `all-tests` profile.

Test gating was fragmented across three overlapping mechanisms:

1. `#[ignore]` attributes on stateful/slow test functions
2. Feature flags (`lightwalletd-grpc-tests`, `zebra-checkpoints`, `indexer`)
3. Environment variable early-returns (`TEST_LARGE_CHECKPOINTS`, `TEST_SYNC_TO_CHECKPOINT`, `TEST_SYNC_PAST_CHECKPOINT`, `TEST_LIGHTWALLETD`)

Adding a new stateful test required changes in four places: the test function, the nextest exclusion denylist, a new nextest profile, and the CI workflow. This made the test system fragile and resistant to contribution.

## Priorities & Constraints

- **Zero-config test addition**: Adding a new test should require placing a function in the right module file — no config changes.
- **Structural discoverability**: Test categories should be visible in the directory structure, not encoded in config files.
- **CI simplification**: Reduce the nextest profile count and eliminate the manual exclusion denylist.
- **Compilation efficiency**: Minimize link-time overhead from multiple test binaries.
- **Stable contracts**: Use module paths as the durable scheduling contract. Function names may stay stable when still accurate, but CI filters must target exact module paths rather than relying on global test-name uniqueness.
- **Feature-gate preservation**: Keep `#[cfg(feature = "...")]` gates for tests that pull in optional compile-time dependencies (tonic/protobuf for lightwalletd, zebra-checkpoints binary).

## Considered Options

1. **Status quo with incremental cleanup**: Keep `acceptance.rs`, add comments, improve the nextest denylist.
2. **Multiple top-level test binaries**: Split into `unit.rs`, `integration.rs`, `stateful.rs` — each compiles as its own binary.
3. **Single binary with module-path categorization**: Use `autotests = false` with one `[[test]]` entry. Organize tests into `unit/`, `integration/`, `stateful/`, and `e2e/` module directories. Use nextest regex filters on module paths.
4. **Two binaries (fast + stateful)**: Separate binary for stateful tests with a `required-features` gate to avoid compiling them on PR runners.

### Pros and Cons of the Options

#### Option 1: Status quo

- Bad: Every new test requires 4-place coordination. The 14-entry denylist keeps growing.

#### Option 2: Multiple top-level binaries

- Good: Clear separation. Each binary can have different compile features.
- Bad: Each `.rs` file in `tests/` re-links the entire `zebrad` crate. Three binaries = three link operations. Nextest filtering works on binary name, not module path — less granular.

#### Option 3: Single binary with modules

- Good: One link operation. Module paths give nextest fine-grained filtering (`test(/^unit::/)`). Adding tests requires zero config changes. Directory structure is self-documenting.
- Bad: All tests always compile (including stateful test code on PR runners). Any change to any module recompiles the binary.

#### Option 4: Two binaries (fast + stateful)

- Good: Stateful tests can be feature-gated to avoid compiling on PR runners. Fast binary stays lean.
- Bad: Two link operations. Shared helpers need to be in a common crate or duplicated. More complex Cargo.toml.

## Decision Outcome

Chosen option: [Option 3: Single binary with module-path categorization]

At Zebra's current scale (~75 integration tests, ~4,400 lines of test code), the compilation overhead of always including stateful and E2E test code is negligible compared to the architectural clarity gained. The single-binary approach eliminates link-time duplication and makes the module hierarchy the single source of truth for test categorization.

The module structure maps directly to four tiers:

```
zebrad/tests/
  main.rs                 # mod common; mod unit; mod integration; mod stateful; mod e2e;
  unit/                   # Fast: CLI, config, end-of-support (<1 min)
  integration/            # Launches zebrad, no cached state (5-15 min)
    sync.rs, rpc.rs, database.rs, regtest.rs, mempool.rs, network.rs
  stateful/               # Requires cached blockchain state (30 min - days)
    sync.rs, rpc.rs, lightwalletd.rs, indexer.rs
  e2e/                    # Full-system public-network flows (hours - days)
    sync.rs, checkpoints.rs, lightwalletd.rs
  common/                 # Shared test helpers (not test functions)
```

`stateful::` and `e2e::` are intentionally separate. `stateful::` means the test
requires pre-existing Zebra or lightwalletd state, usually on a persistent GCP
disk. `e2e::` means the test validates a whole-system public-network flow such
as full sync, large checkpoint sync, checkpoint generation, or lightwalletd full
sync. Some E2E tests also use cached state, but their scheduling risk is the
end-to-end flow, so they live under `e2e::`.

### Nextest configuration

The 23 per-test profiles were replaced with 5 semantic profiles:

| Profile | Purpose | Filter |
|---------|---------|--------|
| `default` | Local dev | `not test(/^stateful::/) and not test(/^e2e::/)` |
| `ci` | PR CI | `not test(/^stateful::/) and not test(/^e2e::/)` |
| `ci-stateful` | GCP VMs | `test(/^stateful::/)`; CI also passes exact `--filter-expr` |
| `ci-e2e` | GCP VMs | `test(/^e2e::/)`; CI also passes exact `--filter-expr` |
| `check-no-git-dependencies` | Release check | `test(=check_no_git_dependencies)` |

Per-test timeout overrides in `ci-stateful` and `ci-e2e` handle the varying
runtime requirements (30 min to 20 days) without one profile per test.

### Env var gate elimination

The scheduling-gate env vars (`TEST_LARGE_CHECKPOINTS`, `TEST_SYNC_TO_CHECKPOINT`, `TEST_SYNC_PAST_CHECKPOINT`) were removed. Tests are now selected structurally: stateful tests live in `stateful::` modules, E2E tests live in `e2e::` modules, and both are excluded by the nextest `default-filter`. The `#[ignore]` attribute remains as a safety net for `cargo test` (without nextest).

`TEST_LIGHTWALLETD` was preserved in helper functions because it checks runtime binary availability (whether `lightwalletd` is installed), not scheduling — a fundamentally different concern.

### Feature flags

Feature flags were preserved for tests that add compile-time dependencies:

- `#[cfg(feature = "lightwalletd-grpc-tests")]` — pulls in tonic/protobuf
- `#[cfg(feature = "zebra-checkpoints")]` — requires the zebra-checkpoints binary
- `#[cfg(feature = "indexer")]` — requires indexer columns

These gates are applied at the module level in `stateful/mod.rs` and `e2e/mod.rs`, so individual test functions don't need them.

### CI workflow changes

- `tests-unit.yml`: Changed `--profile all-tests` to `--profile ci`
- `zfnd-ci-integration-tests-gcp.yml`: Changed per-test `NEXTEST_PROFILE` values to `ci-stateful` or `ci-e2e` with exact module-path `NEXTEST_FILTER` values such as `test(=stateful::sync::sync_update_mainnet)` or `test(=e2e::sync::sync_full_mainnet)`.
- `docker/entrypoint.sh`: Added `NEXTEST_FILTER` env var support alongside existing `NEXTEST_PROFILE`; `ci-stateful` and `ci-e2e` fail fast if the filter is missing.

### Expected Consequences

- Adding a new test requires placing a function in the right module — no nextest or CI config changes.
- The manual exclusion denylist in nextest is eliminated. New stateful and E2E tests are automatically excluded from PR CI by their module path.
- Test discoverability improves: `cargo nextest list -E 'test(/^stateful::/)'` shows all stateful tests, and `cargo nextest list -E 'test(/^e2e::/)'` shows all E2E tests.
- The single binary compiles ~4,400 lines of test code regardless of which tests will run. This is acceptable at current scale but should be revisited if the test suite grows 5-10x.
- `cargo test -p zebrad` (without nextest) will attempt to run non-ignored tests from all categories. The `#[ignore]` attributes on stateful and E2E tests prevent accidental long-running test execution.

## More Information

- [Delete Cargo Integration Tests — matklad](https://matklad.github.io/2021/02/27/delete-cargo-integration-tests.html) — the analysis of why single-binary integration tests compile faster
- [Nextest Per-Test Overrides](https://nexte.st/docs/configuration/per-test-overrides/) — how timeout overrides replace per-test profiles
- [Nextest Filterset Reference](https://nexte.st/docs/filtersets/reference/) — the `test(/regex/)` syntax used for module-path filtering
- reth's `testing/` directory pattern — inspiration for dedicated test infrastructure crates
- Substrate's pallet test organization — inspiration for per-module test categorization

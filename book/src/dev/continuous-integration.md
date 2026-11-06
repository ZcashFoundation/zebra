# Zebra continuous integration

Zebra uses GitHub Actions for pull-request checks, container builds, and
stateful tests on Google Cloud Platform (GCP). See the
[workflow inventory and dependency diagrams](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/README.md);
workflow YAML defines triggers, permissions, and job conditions.

## Required checks

The [main branch ruleset](https://github.com/ZcashFoundation/zebra/rules/746379)
requires these status contexts:

| Required context | Producer | Purpose |
| --- | --- | --- |
| `lint` | [`lint.yml`](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/lint.yml), job `lint` | Rust, dependency, Dockerfile, and shell checks |
| `unit-tests` | [`tests-unit.yml`](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/tests-unit.yml), job `unit-tests` | Linux tests, Windows `zebra-test` tests, and release-only dependency checks |
| `test-crates` | [`test-crates.yml`](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/test-crates.yml), job `test-crates` | Per-crate builds with different features and minimum supported Rust versions |
| `pr-gate-result` | [`pr-gate.yml`](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/pr-gate.yml), job `pr-gate-result` | Pull-request declarations, change fragments, and release readiness |
| `merge-policy` | [`merge-policy.yml`](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/merge-policy.yml), job `merge-policy` | Rejects the `do-not-merge` label |
| `mergefreeze` | Merge Freeze GitHub App | Release-window hold |

The first four contexts aggregate worker results. An aggregator runs with
`if: always()` and uses `re-actors/alls-green` to interpret failures,
cancellations, and explicitly permitted skips. A green aggregator does not
mean every worker ran. The `pr-gate-result` aggregator also permits failures
from `api-diff` and `changelog-gate`; review their findings separately.

### Path filters

The [shared path filters](https://github.com/ZcashFoundation/zebra/blob/main/.github/path-filters.yml)
gate lint, unit-test, and crate-build workers through `dorny/paths-filter`
on PRs and merge groups, while retaining required status reporting.
Pushes to `main` bypass filtering and set worker-selection outputs to `true`.

The filter keys include `lint`, `hadolint`, `shellcheck`, `api_diff`,
`unit_tests`, `test_crates`, `test_docker`, `book`, `coverage`, and
`docs_check`. Edit the shared file rather than duplicating these globs in a
new job. Each filter includes the shared file itself. GCP integration and
node-deployment workflows instead have their own event and path filters;
changing the shared file does not automatically run those workflows.

The `unit_tests` filter also matches three book pages that unit tests compare
with the code: the configuration reference and the network upgrade table
(`zebrad/tests/unit/book.rs`), and the JSON-RPC method table
(`zebra-rpc/src/methods/tests/book.rs`). A change that adds a configuration key
or an RPC method, or changes an activation height, fails these tests until the
book is updated.

On PRs targeting `main`, labels other than `release` do not rerun the main
Linux unit-test job. PRs targeting `next` run it on edits and label events
when the path filter matches; `next` has no merge queue to rerun it.

Add gated workers to the aggregator's `needs` and legitimate skips to
`allowed-skips`; retain `merge_group`. Require a new context only after a
workflow produces it. The crate matrix discovers workspace crates with
`cargo tree`; new crates need no matrix entry.

### Runners and build caches

Linux unit tests use Rust 1.97.0, `ubuntu-26.04`, `cargo nextest` with the
`ci` profile, release mode without link-time optimization, and
`default-release-binaries`. Windows tests cover only `zebra-test` on
`windows-latest`.

Reuse the [build setup action](https://github.com/ZcashFoundation/zebra/blob/main/.github/actions/setup-zebra-build/action.yml)
for Linux Rust jobs needing Clang, Protocol Buffers, or system RocksDB;
it installs them and sets `ROCKSDB_LIB_DIR=/usr/lib/`.

Follow the
[Rust cache policy and inspection commands](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/README.md#5-rust-build-caching)
when adding jobs, changing keys, or deleting caches.

The reusable container builder defaults to `ubuntu-26.04` for AMD64 and
`ubuntu-24.04-arm` for ARM64. Repository variables
`DOCKER_BUILD_RUNNER_AMD64` and `DOCKER_BUILD_RUNNER_ARM64` override them.
Runtime images build for both architectures; test images build for AMD64.
The `DOCKER_BUILDER` variable selects Docker Build Cloud when set to
`cloud`; otherwise the default is the `docker-container` driver. The
`no_cache` dispatch input disables Docker build-cache reads, not Rust
caches or cached blockchain state.

## Integration tests

### GCP stateful tests

The [Integration Tests on GCP workflow](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zfnd-ci-integration-tests-gcp.yml)
does not run on every PR update:

| Event | Selection |
| --- | --- |
| Pull request targeting `main` | `labeled`, `synchronize`, or `reopened`; the head must be in the repository and the PR must carry `run-stateful-tests` |
| Push to `main` | Rust, dependency, test-data, Docker, or explicitly listed integration-workflow paths |
| Schedule | Friday at 12:00 UTC, only in `ZcashFoundation/zebra` |
| Manual dispatch | Selected ref and network, with optional cache-regeneration or full-sync inputs |

The workflow has no `merge_group` trigger and does not gate merges.
For fork coverage, review the code before copying it to a repository-owned
branch and dispatching the secret-bearing workflow or labeling its PR.

Run the ordinary suite on a repository-owned branch:

```sh
gh workflow run zfnd-ci-integration-tests-gcp.yml \
  --repo ZcashFoundation/zebra --ref my-branch -f network=Mainnet
```

Select `regenerate-disks=true` to rebuild checkpoint state,
`run-full-sync=true` to rebuild tip state for the selected network, or
`run-lwd-sync=true` for a Mainnet lightwalletd full sync. These inputs retain
cache prerequisites.

Tests cover full validation beyond the mandatory checkpoint, tip updates,
checkpoint generation, and RPCs. Full Zebra syncs run weekly, on request,
or when matching tip state is missing. Mainnet lightwalletd update,
transaction, and gRPC wallet jobs run on schedules, manual dispatches, and
eligible labeled PRs, not `main` pushes. Both Testnet jobs pass
`allow_failure: true` to the reusable workflow.

The [test deployment workflow](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zfnd-deploy-integration-tests-gcp.yml)
runs tests in a GCP VM container; `test-result` uses self-hosted
`zfnd-runners` to supervise and collect logs. Timeouts are 180 minutes,
or 7,200 minutes for `is_long_test`. The entrypoint uses `NEXTEST_PROFILE`
and `NEXTEST_FILTER`; `ci-stateful` and `ci-e2e` require an explicit filter.

GCP jobs request `id-token: write` and authenticate through Workload
Identity Federation using `GCP_WIF` and the appropriate service-account
variable. Builds use `GCP_ARTIFACTS_SA`; VM and disk operations use
`GCP_DEPLOYMENTS_SA`. Container builds receive `DOCKERHUB_USERNAME` and
`DOCKERHUB_TOKEN`; test supervision receives `GCP_SSH_PRIVATE_KEY`. Approving
a fork workflow does not provide these repository secrets.

### Cached blockchain state

The [cached-disk workflow](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zfnd-find-cached-disks.yml)
searches Compute Engine images using the database major version from
`zebra-state/src/constants.rs` and the lowercased network. It reports Zebra
checkpoint, Zebra tip, and combined lightwalletd/Zebra tip availability.
Tests create private disks from the selected image.

When state saving is selected, `create-state-image` runs after successful
`test-result`. It saves updated images when the height exceeds the input
image by more than 576 blocks; full-sync images and `force_save_to_disk=true`
bypass that threshold. See [image selection](gcp-deployment-operations.md#cache-images).

### Cross-project interoperability tests

The separate [Trigger Integration Tests workflow](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/trigger-integration-tests.yml)
sends a `zebra-interop-request` repository-dispatch event to
[zcash/integration-tests](https://github.com/zcash/integration-tests). It runs
on Mondays at 06:00 UTC and on manual dispatch, not automatically on PRs.
Its GitHub App token comes from `Z3_APP_ID` and `Z3_APP_PRIVATE_KEY`.

To test a PR's head commit, including a fork PR, dispatch from `main` with
its PR number:

```sh
gh workflow run trigger-integration-tests.yml \
  --repo ZcashFoundation/zebra --ref main -f pr_number=12345
```

Replace `12345` with the PR number; omit `pr_number` to send the selected
ref's commit. Inspect results in the integration-tests repository: a
successful dispatch does not mean its tests passed.

## Benchmarks

The [benchmark workflow](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/benchmarks.yml)
runs six targets listed in `BENCHES`. Register new targets there and in the
crate's `Cargo.toml` with `[[bench]]` and `harness = false`. Manual dispatch
runs `cargo-criterion` and publishes results to `gh-pages/dev/bench`. The
[book workflow](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/book.yml)
includes that dashboard in the documentation deployment. The alert
threshold is 150% of the previous result; alerts do not fail the job.

PRs labeled `run-benchmarks` run base and head targets on label events and
subsequent pushes, save Criterion baselines, and post the `critcmp` comparison
to the run summary and a PR comment. Failed or missing base targets appear
in the summary without a comparison. See
[benchmark invocation and results](profiling-and-benchmarking.md).

## Coverage

The [coverage workflow](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/coverage.yml)
sets `PROPTEST_CASES=1` and `PROPTEST_MAX_SHRINK_ITERS=0`. These reduced runs
do not replace [property testing](proptests.md).

## Automated merges

A maintainer selects **Merge when ready** after the PR meets the review and
check requirements. GitHub's merge queue tests a cumulative merge result
against `main`; a PR need not be up to date before enrollment. Inspect the
`merge_group` run when a queue entry fails, not only the source PR's runs.
PRs targeting [`next`](next-branch.md) merge directly after their required
checks; they do not use this queue.

### Queue configuration

The `PR Requirements` ruleset requires one approving review and resolved
review conversations. See the
[queue settings](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/README.md#6-queue-management)
and [repository rules](https://github.com/ZcashFoundation/zebra/settings/rules)
before changing policy.

The `merge-policy` check rejects `do-not-merge` on both a PR and the PRs
identified in a merge-group ref. The external `mergefreeze` status is a
separate hold; the `release` label does not bypass it. Manage freezes and
release exceptions through [Merge Freeze](https://docs.mergefreeze.com/github-merge-queue).
For an immediate freeze, remove active non-release queue entries rather
than relying on a status already reported for a merge group being revoked.

Repository administrators change required contexts in the ruleset. Keep
each context pinned to its producing app. Document any emergency bypass on
the PR and restore normal enforcement after repairing the branch.

## Troubleshooting

Start with the earliest failed build or test, not a downstream aggregation
or missing-output error. For stateful tests, inspect the selected image,
VM creation, container logs, test exit status, image creation, and cleanup
in order. Rerun the complete integration workflow when its VM has been
removed; rerunning only a later job does not recreate its prerequisites.

For a sync timeout, inspect peer connectivity, cached-state height, and
recent performance changes before extending the timeout. The
[checkpoint procedure](zebra-checkpoints.md) covers updating checkpoints.
Do not treat a repeated test failure as a transient infrastructure error.

### Fixing duplicate dependencies in `Check deny.toml bans`

The [lint workflow](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/lint.yml)
checks bans, sources, advisories, and licenses with default features,
`default-release-binaries`, and all features. Its all-features jobs use
`--allow banned` because that set activates optional OpenSSL; the other
feature sets use `--allow unmatched-skip-root`. Advisory failures are
non-blocking.

The bans jobs are named `Check deny bans` with a feature-selection suffix.
Use `cargo tree -d` to locate duplicate versions and
`cargo tree -i crate@version` to find their source. Prefer compatible
dependency updates or removing unused features over an exception.

When an exception is necessary, document its cause in
[`deny.toml`](https://github.com/ZcashFoundation/zebra/blob/main/deny.toml).
Remove obsolete `skip-tree` entries when the all-features bans check
reports an unmatched skip root:

```sh
cargo deny --all-features check bans
```

### Resource failures

For full disks, distinguish GitHub runner storage, Docker build storage,
and GCP state disks. Integration VMs request 400 GB `pd-balanced` disks.
Changing the creation size does not resize an existing persistent disk.

Inspect the failing branch's Rust cache before deleting anything. Avoid
clearing all repository caches to address one failure. GCP resource
retention and node recovery procedures are in the
[deployment runbook](gcp-deployment-operations.md#daily-cleanup).

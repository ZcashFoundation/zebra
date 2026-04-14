# Zebra Continuous Delivery

The continuous-delivery pipeline deploys every commit merged to `main` to a staging environment, and every published release to production, on Google Cloud Platform.

## Lifecycle classes

The pipeline distinguishes three lifecycle classes by trigger. Each class has its own GCP project, MIG, and stateful disk. Rationale and trade-offs live in [ADR 0006](../../../docs/decisions/devops/0006-gcp-deployment-lifecycle.md); operational procedures live in the [operations runbook](gcp-deployment-operations.md).

| Class       | Trigger              | Project           | MIG                            | Stateful disk                       | Lifetime                                                   |
| ----------- | -------------------- | ----------------- | ------------------------------ | ----------------------------------- | ---------------------------------------------------------- |
| **Prod**    | `release`            | `zfnd-prod-zebra` | `zebrad-${network}`            | `zebrad-cache-${network}`           | Permanent. Rolling template swap on every release.         |
| **Staging** | `push` to `main`     | `zfnd-dev-zebra`  | `zebrad-main-${network}`       | `zebrad-cache-main-${network}`      | Permanent. Rolling template swap on every commit.          |
| **Canary**  | `workflow_dispatch`  | `zfnd-dev-zebra`  | `zebrad-${branch}-${network}`  | `zebrad-cache-${branch}-${network}` | Per-branch. Manually reaped via labels (see runbook).      |

For each network (`mainnet`, `testnet`), the MIG runs one Spot instance per zone in the configured region, typically three zones in `us-east1`. The stateful disk persists the chain across template swaps, so a release upgrade does not trigger a multi-day resync.

## Update mechanics

Every trigger runs the same flow:

1. Build a new instance template containing the commit's container image.
2. If the MIG already exists, run a rolling template swap with `--max-unavailable=1`. The other zones keep serving while one instance is replaced.
3. If the MIG does not exist, create it. The stateful disk bootstraps from the latest matching cache image found by `find-cached-disks`, so the new MIG does not start from genesis.
4. Apply per-instance configurations to assign static IPs.

Integration tests produce cache images in `zfnd-ci-integration-tests-gcp.yml`'s `create-state-image` job. The image name encodes the branch, commit, state-DB version, network, and timestamp. The lookup falls back from the current branch to `main` to any branch.

## Failure handling

The deploy workflow opens an issue (label `S-ci-fail-release-auto-issue`) when a deploy fails on `main` or a release. The same issue collects subsequent failures and must be closed manually after recovery. The runbook covers common failure modes (`RESOURCE_IN_USE_BY_ANOTHER_RESOURCE`, `wait-until --stable` timeouts, disk corruption).

## Triggers

The workflow runs on:

- a `push` to `main` that touches Rust code, dependencies, Docker files, or the workflow itself,
- a published `release`, or
- a `workflow_dispatch` from any branch. The dispatcher chooses the environment (`dev` by default, `prod` if selected explicitly).

Pull requests run only the Docker-configuration tests; they do not deploy.

For implementation details, see the [deploy workflow](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zfnd-deploy-nodes-gcp.yml).

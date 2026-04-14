# Zebra Continuous Delivery

The continuous-delivery pipeline deploys every commit merged to `main` to a staging environment and every published release to production, on Google Cloud Platform.

## Topology: one zonal MIG per (environment, branch, network, zone)

The pipeline targets two GCP environments. Each network in each environment deploys to three zonal Managed Instance Groups (MIGs) in `us-east1` zones `b`, `c`, and `d`. Each zonal MIG holds one Zebra instance with one stateful cache disk and one static IP.

| Trigger              | Environment          | MIGs per network | MIG name                                      | Stateful disk                                 |
| -------------------- | -------------------- | ---------------- | --------------------------------------------- | --------------------------------------------- |
| `release`            | `zfnd-prod-zebra`    | 3 (one per zone) | `zebrad-${network}-${zone-letter}`            | `zebrad-cache-${network}-${zone-letter}`      |
| `push` to `main`     | `zfnd-dev-zebra`     | 3 (one per zone) | `zebrad-main-${network}-${zone-letter}`       | `zebrad-cache-main-${network}-${zone-letter}` |
| `workflow_dispatch`  | `zfnd-dev-zebra`     | 1 (user-chosen zone) | `zebrad-${branch}-${network}-${zone-letter}` | `zebrad-cache-${branch}-${network}-${zone-letter}` |

ADR [0006](../../../docs/decisions/devops/0006-gcp-deployment-naming.md) records the rationale; the [runbook](gcp-deployment-operations.md) covers day-to-day procedures.

## Update mechanics

Each push and each release fans out to six `deploy-nodes` jobs (2 networks × 3 zones). A workflow_dispatch is a single job (user picks the zone). Every job runs the same flow for its zonal MIG:

1. Build a new instance template with the commit's container image.
2. Ensure the zonal stateful disk exists. On first deploy, create it from the latest matching cache image. On subsequent deploys, attach the existing disk.
3. If the zonal MIG exists, run `rolling-action start-update --max-unavailable=1`. True per-zone rolling: this zone's MIG replaces its instance while the other two zones keep serving. The stateful disk persists across the replace.
4. If the zonal MIG does not exist, create it with `--size=1` and apply the stateful policy.
5. Assign the static IP (push and release only; workflow_dispatch uses ephemeral). Zone-to-IP mapping is deterministic: zone `b` → primary, zone `c` → secondary, zone `d` → tertiary.

Cache images come from `zfnd-ci-integration-tests-gcp.yml`'s `create-state-image` job. Image names encode branch, commit, state-DB version, network, and timestamp. One image per network seeds all three zones. Lookup priority in `gcp-get-cached-disks.sh`: current branch, then `main`, then any branch; most recent first.

Deploy success has two channels: `deploy-nodes` reports infrastructure, `verify-nodes` reports application health. See the [runbook](gcp-deployment-operations.md#deploy-success-has-two-channels) for details.

## Triggers

The workflow runs on:

- a `push` to `main` that touches Rust code, dependencies, Docker files, or the workflow itself
- a published `release`
- a `workflow_dispatch` from any branch (dispatcher picks `network`, `zone`, and `environment`)

Pull requests run only the Docker-configuration tests; they do not deploy.

For implementation details, see the [deploy workflow](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zfnd-deploy-nodes-gcp.yml).

# Zebra Continuous Delivery

The continuous-delivery pipeline deploys every commit merged to `main` to the `stage` environment and every published release to `prod`, on Google Cloud Platform. PR-triggered work uses the `dev` environment.

## Topology: one zonal MIG per (environment, branch, network, zone)

The pipeline targets two GCP environments. Each network in each environment deploys to three zonal Managed Instance Groups (MIGs) in `us-east1` zones `b`, `c`, and `d`. Each zonal MIG holds one Zebra instance with one stateful cache disk and one static IP.

| Trigger              | Environment label | GCP project       | MIGs per network | MIG name                                      | Stateful disk                                 |
| -------------------- | ----------------- | ----------------- | ---------------- | --------------------------------------------- | --------------------------------------------- |
| `release`            | `prod`            | `zfnd-prod-zebra` | 3 (one per zone) | `zebrad-${network}-${zone-letter}`            | `zebrad-cache-${network}-${zone-letter}`      |
| `push` to `main`     | `stage`           | `zfnd-dev-zebra`  | 3 (one per zone) | `zebrad-main-${network}-${zone-letter}`       | `zebrad-cache-main-${network}-${zone-letter}` |
| `workflow_dispatch`  | `dev` or `prod`   | selected by env   | 1 (user-chosen zone) | `zebrad-${branch}-${network}-${zone-letter}` | `zebrad-cache-${branch}-${network}-${zone-letter}` |

ADR [0006](../../../docs/decisions/devops/0006-gcp-deployment-naming.md) records the rationale; the [runbook](gcp-deployment-operations.md) covers day-to-day procedures.

## Update mechanics

The event-driven workflow builds an image with its trigger-specific inputs, then passes the complete immutable GAR `repository@sha256:...` identity to the reusable deployment workflow. Release automation can call that same deployment boundary with an image prepared earlier. Each push and each release fans out to six `deploy-nodes` jobs (2 networks × 3 zones), while a workflow_dispatch runs one job for the selected zone. Every job runs the same flow for its zonal MIG:

1. Build a new instance template with the commit's container image.
2. Ensure the zonal stateful disk exists. On first deploy, create it from the latest matching cache image. On subsequent deploys, attach the existing disk.
3. If the zonal MIG exists, run `rolling-action start-update --max-unavailable=1`. True per-zone rolling: this zone's MIG replaces its instance while the other two zones keep serving. The stateful disk persists across the replace.
4. If the zonal MIG does not exist, create it with `--size=1` and apply the stateful policy.
5. Assign the static IP when the caller passes `use_reserved_ip`: always for `push` and `release`, and for a workflow_dispatch that selects `prod` or runs from the `main` ref. Other dev deployments get no external address (`--no-address`). Zone-to-IP mapping is deterministic: zone `b` → primary, zone `c` → secondary, zone `d` → tertiary.

Cache images come from `zfnd-deploy-integration-tests-gcp.yml`'s `create-state-image` job. Image names encode branch, commit, state-DB version, network, and timestamp. One image per network seeds all three zones. Lookup priority in `gcp-get-cached-disks.sh`: current branch, then `main`, then any branch; most recent first.

Each `deploy-nodes` cell holds its per-MIG concurrency lock through template rollout and optional application-health verification. See the [runbook](gcp-deployment-operations.md#deploy-success-has-two-stages) for details.

## Triggers

The workflow runs on:

- a `push` to `main` that touches Rust code, dependencies, Docker files, or the workflow itself
- a published `release`
- a `workflow_dispatch` from any branch (dispatcher picks `network`, `zone`, and `environment`)

Pull requests run only the Docker-configuration tests; they do not deploy.

For implementation details, see the [deploy workflow](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zfnd-deploy-nodes-gcp.yml).

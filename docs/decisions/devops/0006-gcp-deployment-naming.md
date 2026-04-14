---
status: accepted
date: 2026-04-14
builds-on: [Continuous Delivery](../../../book/src/dev/continuous-delivery.md)
story: Stateful disk collisions during release rollouts, silent main-branch CD failures, and the regional-stateful-MIG quirks that made rolling updates impossible on Google Cloud Platform.
---

# GCP Deployment Topology: zonal MIG per (environment, branch, network, zone)

## Context and Problem Statement

Zebra runs as a stateful node: chain state on disk represents many hours or days of synchronization. The continuous-delivery pipeline must update the running container without re-syncing from genesis on every release. On Google Cloud Platform, this constraint is satisfied by attaching a persistent disk (PD) to a Managed Instance Group (MIG) under a stateful policy, so the disk survives instance recreation during rolling updates.

A read-write PD can attach to only one instance at a time. The first design used a single **regional** MIG per `(environment, network)` holding three instances, one per zone, each attached to a same-named zonal disk. Two architectural problems arose from that shape:

1. **Two MIGs cannot share a disk.** Encoding the major Zebra version in the MIG name (`zebrad-v${MAJOR}-${network}`) meant each major release spun up a competing MIG. The platform cannot atomically transfer the stable disk from one MIG to another; the new MIG retried instance creation until one of the two won a zone.
2. **Regional stateful MIGs cannot do per-zone rolling.** GCP requires `--max-surge=0` (no extra capacity with a single-writer disk) and `--max-unavailable` to be either 0 or at least the zone count. A three-zone regional MIG can only update all-at-once or not at all. Empirically, the workflow's `--max-unavailable=1` was silently invalid.

Operating a multi-instance regional MIG also coupled deploy-landed to node-healthy (a single `wait-until --stable` covered both, so testnet's slow peer warmup timed out deploys that had actually landed), conflated network failures under matrix fail-fast, and leaked mainnet cache images into the testnet deploy path because the cache lookup ran once at the workflow level.

## Priorities & Constraints

- Chain state must persist across releases, including major-version upgrades. A multi-day resync is unacceptable.
- A failed deploy must surface a clear, per-zone, per-network error within minutes.
- A developer must be able to deploy a PR branch to a single zone in dev without colliding with other deploys.
- Updates must be true rolling: one zone at a time, others keep serving.
- Cleanup must be auditable and reversible: label-based protection, manual reap commands, no accidental destruction of live stateful disks.
- Cache images produced by integration tests must remain the cold-start mechanism for fresh deploys; cache images are network-scoped (same image seeds all zones).
- The architecture must compose with the existing `release:published` trigger contract.
- The naming model must reuse existing label vocabulary.

## Considered Options

- **Option 1: Regional MIG per `(environment, network)`, versioned MIG names** (original). Each major release spawns a new MIG; disks shared across versions; rolling updates are all-at-once.
- **Option 2: Regional MIG per `(environment, network)`, stable MIG name, branch-derived disk** (first refactor, PR #10482). Fixes the versioned-MIG collision but keeps regional-stateful constraints (all-at-once updates, no per-zone rolling, `wait-until --stable` covering both deploy and health).
- **Option 3: Zonal MIG per `(environment, branch, network, zone)`, each MIG holds 1 instance with 1 stateful disk.** Three zonal MIGs per network per environment for push/release; one zonal MIG for workflow_dispatch. Rolling updates are per-zone with `--max-unavailable=1`. Deploy-landed and node-healthy are separate signals.

### Pros and Cons of the Options

#### Option 1: Regional versioned MIG (status quo, replaced by #10482)

- Bad, because every major release recreates a single-writer disk collision.
- Bad, because no automatic mechanism retires the previous major-version MIG.
- Bad, because a fall-through in trigger-to-disk routing leaked staging and PR deploys into the prod-disk path.
- Good, because each major version had clearly separable infrastructure.

#### Option 2: Regional stable MIG, branch-derived disk

- Good, because the MIG is stable per network, so cross-major-version upgrades are `set-instance-template` plus `rolling-action`.
- Bad, because regional stateful MIGs cannot do per-zone rolling (all-at-once updates, significant downtime window).
- Bad, because `--max-unavailable=1` is platform-rejected; only `0` or `zone_count` are valid. Either no update or all zones down.
- Bad, because `wait-until --stable` couples deploy success to instance health, and testnet peer warmup regularly exceeds any reasonable timeout.
- Bad, because matrix fail-fast default cancels one network when the other fails.
- Good, because this was a large improvement over Option 1 for the versioning problem.

#### Option 3: Zonal MIG per (environment, branch, network, zone)

- Good, because the MIG is the instance: 1:1:1:1 between MIG, instance, stateful disk, static IP. No "regional disks with the same name across zones" ambiguity.
- Good, because `--max-unavailable=1` is natively valid (single-instance MIG). Updates are true per-zone rolling: one zonal MIG replaces its instance while the sister zonal MIGs keep serving.
- Good, because per-zone failures are isolated: one zone's warmup slowness doesn't block the others, and the matrix's `fail-fast: false` gives six independent (network, zone) status cells on the GitHub UI.
- Good, because deploy-landed (`--version-target-reached`) and node-healthy (`--stable` in a separate `verify-nodes` job) are already distinct signals with distinct failure labels.
- Good, because `find-cached-disks` runs per-network (one lookup per network, image seeds all three zones).
- Neutral, because the resource count grows from 2 MIGs to 6 per environment. Fewer resources per MIG (1 instance, 1 disk, 1 IP each) keeps each MIG's blast radius tiny.
- Bad, because migrating from regional-stateful to zonal is a one-shot operator-driven migration per environment.

## Decision Outcome

Chosen option: **Option 3 (zonal MIG per `(environment, branch, network, zone)`)**. The migration from the previous regional-stateful architecture is documented as a one-shot procedure in the runbook.

### Naming

One MIG per matrix cell. Names encode every identity axis:

| Trigger              | Environment          | MIG                                      | Disk                                          |
| -------------------- | -------------------- | ---------------------------------------- | --------------------------------------------- |
| `release`            | `zfnd-prod-zebra`    | `zebrad-${network}-${zone-letter}`       | `zebrad-cache-${network}-${zone-letter}`      |
| `push` to `main`     | `zfnd-dev-zebra`     | `zebrad-main-${network}-${zone-letter}`  | `zebrad-cache-main-${network}-${zone-letter}` |
| `workflow_dispatch`  | `zfnd-dev-zebra`     | `zebrad-${branch}-${network}-${zone-letter}` | `zebrad-cache-${branch}-${network}-${zone-letter}` |

`zone-letter` is `b`, `c`, or `d` (the last segment of `us-east1-b`, etc.). Push and release fan out to six zonal MIGs (2 networks × 3 zones). A `workflow_dispatch` deploys a single zonal MIG with user-selected network and zone (default `us-east1-b`).

### Update mechanics

- Every trigger runs per-cell: `rolling-action start-update` on an existing zonal MIG with `--max-unavailable=1 --max-surge=0 --replacement-method=recreate`. True per-zone rolling.
- Fresh MIG creation pre-creates the zonal disk from the network-scoped cache image, then the template attaches it via `--disk=name=…` (not `--create-disk`). This decouples "disk populated from image" from "MIG attaches disk" and handles both fresh deploys and manual pre-seeding identically.
- The MIG's health check (`/healthy`, `--initial-delay=3600`) governs autohealing tolerance.
- Deploy-landed waits `--version-target-reached --timeout=600` (template rollout done). Verify-nodes waits `--stable --timeout=5400` (peer mesh + warmup complete). Distinct failure labels: `S-ci-fail-release-auto-issue` for infrastructure, `S-ci-fail-verify-auto-issue` for slow warmup.

### Static IPs

Deterministic zone-to-IP mapping for push and release:

- `us-east1-b` → `zebra-${network}` (primary)
- `us-east1-c` → `zebra-${network}-secondary`
- `us-east1-d` → `zebra-${network}-tertiary`

Workflow_dispatch deploys use ephemeral IPs (PR smoke tests don't need stable external addresses).

### Labels

Every MIG, instance, and disk carries `app`, `environment`, `network`, `zone`, `created_by`, `github_ref`, `github_sha`. The `created_by` label (`release`, `push`, or `workflow_dispatch`) discriminates the three deploy kinds for cleanup and inspection. PR-deploy cleanup uses `keep_until=YYYY-MM-DD` and `delete_protection=true` opt-out labels.

### Accepted trade-offs

- Breaking migration from regional to zonal: snapshot-based one-shot procedure in the runbook.
- More MIGs per environment (6 vs 2), each simpler.
- Backwards-incompatible RocksDB format changes still need a snapshot-based handoff at release time; runbook covers it.
- The `cos-stable` + `gce-container-declaration` deploy pattern is deprecated by Google; a future ADR will address it.

## More Information

- [GCP Deployment Operations runbook](../../../book/src/dev/gcp-deployment-operations.md): PR-deploy cleanup, disk-corruption recovery, DB-format-version-break release procedure, regional-to-zonal migration procedure.
- [Continuous Delivery overview](../../../book/src/dev/continuous-delivery.md): the high-level model.
- GCP documentation on [zonal MIGs](https://cloud.google.com/compute/docs/instance-groups/zonal-migs), [stateful MIGs](https://cloud.google.com/compute/docs/instance-groups/stateful-migs), and [persistent disk attachment limits](https://cloud.google.com/compute/docs/disks#pd_modes).

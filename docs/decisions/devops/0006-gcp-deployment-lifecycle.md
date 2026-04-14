---
status: accepted
date: 2026-04-14
builds-on: [Continuous Delivery](../../../book/src/dev/continuous-delivery.md)
story: Stateful disk collisions during release rollouts and silent main-branch CD failures on Google Cloud Platform.
---

# GCP Deployment Lifecycle: stable MIG per network with three-class naming

## Context and Problem Statement

Zebra runs as a stateful node: chain state on disk represents many hours or days of synchronization. The continuous-delivery pipeline must update the running container without re-syncing from genesis on every release. On Google Cloud Platform, this constraint is satisfied by attaching a persistent disk (PD) to a Managed Instance Group (MIG) under a stateful policy, so the disk survives instance recreation during rolling updates.

A read-write PD can only be attached to one instance at a time. Any deployment design that places two MIGs in front of the same disk creates a single-writer race that the platform cannot resolve. The deploy workflow's previous design encoded the major version in the MIG name (`zebrad-v${MAJOR}-${network}`) while keeping a stable disk name (`zebrad-cache-${network}`); each major release created a new MIG that competed with its predecessor for the existing disk. The same workflow used a shell conditional to pick the disk name per trigger, which silently fell through and routed every event to the same disk.

The prior model thus coupled three independent concerns (MIG identity, container image version, disk identity) and relied on shell-conditional fragility to keep them separate. The combination produced predictable collisions on every major release and indistinct failure modes on routine pushes.

## Priorities & Constraints

- Chain state must persist across releases, including major-version upgrades. A multi-day resync is unacceptable.
- A failed deploy must surface a clear error within minutes, not after a 20-minute `wait-until --stable` timeout.
- Developers must be able to run `workflow_dispatch` deploys from a PR branch without colliding with the staging deploy or with other developers' canary deploys.
- Cleanup must be auditable and reversible: developers must be able to mark a canary "do not delete" with an explicit expiry, and the standard cleanup must never destroy a production stateful disk.
- Cache images produced by integration tests must remain the cold-start mechanism for fresh deploys.
- The architecture must compose with the existing `release:published` trigger contract (downstream automation depends on it).

## Considered Options

- **Option 1: Versioned MIG names per major version (status quo).** Each major release creates a new MIG that fights its predecessor for the stable disk. Manual choreography is required to migrate disks across major releases.
- **Option 2: Stable MIG per network, rolling template swap on every trigger.** The MIG identity is the stable name; the major version becomes metadata in the template. Cross-version transitions become `set-instance-template` plus `rolling-action` on the existing MIG.
- **Option 3: Blue-green with snapshot-based handoff.** Snapshot the live disks, create the new MIG with new disks from snapshot, switch traffic, delete the old MIG.

### Pros and Cons of the Options

#### Option 1: Versioned MIG names

- Bad, because every major release recreates a single-writer disk collision that the platform cannot resolve automatically.
- Bad, because no automatic mechanism retires the previous major-version MIG, so old MIGs accumulate indefinitely.
- Bad, because a fall-through in trigger-to-disk routing also routed staging and canary deploys into the prod-disk path, multiplying the collision surface.
- Good, because each major version had clearly separable infrastructure for audit purposes.

#### Option 2: Stable MIG per network, rolling template swap

- Good, because the disk is the identity. Exactly one MIG per network per environment updates its template on every trigger; no inter-MIG handoff is ever needed.
- Good, because rolling updates with `--max-unavailable=1` keep the other zones serving while one zone is replaced. Per-zone downtime is bounded by Zebra start plus RocksDB replay (minutes).
- Good, because the same machinery applies to all three lifecycle classes (prod, staging, canary), distinguished only by MIG name and project.
- Good, because failure modes simplify: a stuck rolling update blocks one MIG instead of creating a parallel MIG.
- Bad, because a backwards-incompatible RocksDB format change cannot be done in place; that case requires the snapshot-based handoff (Option 3) as a one-shot operator procedure.
- Neutral, because the MIG name no longer carries the major version (cosmetic only).

#### Option 3: Blue-green with snapshot-based handoff

- Good, because a crash-loop on the new version is recoverable by switching back to the previous MIG.
- Good, because it is the right answer for the rare case of a backwards-incompatible RocksDB format change.
- Bad, because storage cost doubles during cutover and snapshots are only crash-consistent.
- Bad, because every release requires manual choreography. Developer experience is poor for the common case.

## Decision Outcome

Chosen option: **Option 2 (stable MIG per network, rolling template swap)** as the default for every trigger, with **Option 3 reserved as a documented runbook procedure** for the rare DB-format-version break.

Three lifecycle classes:

| Class       | Trigger              | Project           | MIG                            | Disk                                |
| ----------- | -------------------- | ----------------- | ------------------------------ | ----------------------------------- |
| **Prod**    | `release`            | `zfnd-prod-zebra` | `zebrad-${network}`            | `zebrad-cache-${network}`           |
| **Staging** | `push` to `main`     | `zfnd-dev-zebra`  | `zebrad-main-${network}`       | `zebrad-cache-main-${network}`      |
| **Canary**  | `workflow_dispatch`  | `zfnd-dev-zebra`  | `zebrad-${branch}-${network}`  | `zebrad-cache-${branch}-${network}` |

The stateful policy keeps `auto-delete=on-permanent-instance-deletion`. Lifecycle to MIG/disk mapping is computed by a `case` on `github.event_name` rather than a shell conditional, removing the fall-through risk. The rolling action uses `--max-unavailable=1` for true rolling.

A pre-flight squatter check runs before every MIG create or update. The check queries whether the target stateful disk is held by an instance from a different MIG and fails fast with the squatter's name, replacing the prior 20-minute `wait-until --stable` timeout for this failure class.

Every deploy stamps labels on its instance template, which propagate to instances and disks: `lifecycle_class`, `created_by`, `github_ref`, and `github_sha`. These labels make per-developer and per-class filters trivial in cleanup and inspection.

The canary lifecycle is **manual**. Operators apply labels:

- `keep_until=YYYY-MM-DD` to mark a canary as "do not delete before this date".
- `delete_protection=true` for indefinite preservation, requiring manual reset.

The runbook documents the `gcloud` filters that respect these labels. The policy is ritualized rather than mechanical, in line with existing team practice.

### Expected Consequences

Positive:

- Cross-major-version upgrades become a non-event: `set-instance-template` plus `rolling-action` on the existing MIG. Two MIGs never exist for the same network in the same environment.
- The class of silent fall-through failures disappears: trigger-to-disk routing is explicit in the workflow, and the pre-flight check fails fast with actionable text.
- Developer experience for canaries: dispatch from a PR branch creates `zebrad-${branch}-${network}` without colliding with `main` or with other branches. Filters like `--filter="labels.created_by=workflow_dispatch AND labels.github_ref=my-branch"` make resources discoverable and reapable.

Accepted trade-offs:

- Breaking change for existing infrastructure: prior versioned MIGs (`zebrad-v${MAJOR}-${network}`) are removed. A one-shot operator-driven migration handles the transition with snapshot-based recovery.
- Backwards-incompatible RocksDB format changes still require manual snapshot-based handoff. The runbook covers the procedure.
- The `cos-stable` plus `gce-container-declaration` deploy pattern that Zebra uses is deprecated by Google. A future ADR will plan the migration to a non-deprecated container-on-VM pattern; this ADR does not address it.

## More Information

- [GCP Deployment Operations runbook](../../../book/src/dev/gcp-deployment-operations.md): canary cleanup, disk-corruption recovery, DB-format-version-break release procedure.
- [Continuous Delivery overview](../../../book/src/dev/continuous-delivery.md): the high-level model.
- GCP documentation on [stateful MIGs](https://cloud.google.com/compute/docs/instance-groups/stateful-migs) and [persistent disk attachment limits](https://cloud.google.com/compute/docs/disks#pd_modes).

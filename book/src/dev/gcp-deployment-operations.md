# GCP Deployment Operations

Operational procedures for the GCP Continuous Delivery pipeline. Architectural rationale: [ADR 0006](../../../docs/decisions/devops/0006-gcp-deployment-naming.md). High-level model: [Continuous Delivery](continuous-delivery.md).

Two GCP projects (`zfnd-prod-zebra`, `zfnd-dev-zebra`) in `us-east1`, zones `b`, `c`, `d`. Every MIG, instance, and disk carries `environment`, `network`, `zone`, `created_by`, `github_ref`, and `github_sha` labels. Use `created_by` as the kind discriminator: `release` (production), `push` (staging), `workflow_dispatch` (PR deploy).

The recipes below assume `gh` and `gcloud` are authenticated, and use these shell defaults when scoped to dev:

```bash
P=zfnd-dev-zebra
```

For production, set `P=zfnd-prod-zebra` instead.

Zonal MIGs use `--zone` (not `--region`). One MIG per zone per network. MIG names end with the zone letter (`-b`, `-c`, `-d`).

## Deploy success has two channels

The workflow reports two independent signals:

- `deploy-nodes` (and `failure-issue` with label `S-ci-fail-release-auto-issue`) reports **infrastructure**: template, zonal MIG, stateful disk, and static IP landed. Usually green within 3-5 minutes per matrix cell; a failure means the deploy itself broke.
- `verify-nodes` (and `verify-failure-issue` with label `S-ci-fail-verify-auto-issue`) reports **application**: the zonal MIG reached HEALTHY. Up to 90 minutes; a failure means the node took longer than that to establish peers and catch up to chain tip. The MIG itself is fine; on-call action is usually "wait, or investigate why sync is slow".

## Quick reference

| Goal                                                 | Section                                                       |
| ---------------------------------------------------- | ------------------------------------------------------------- |
| Smoke-test a PR branch / find / label / reap         | [PR deploys](#pr-deploys)                                     |
| Diagnose a deploy that does not converge             | [Diagnose a stuck MIG](#diagnose-a-stuck-mig)                 |
| Recover from a corrupted cache disk                  | [Recover a corrupted cache disk](#recover-a-corrupted-cache-disk) |
| Cut a release with a backwards-incompatible DB format | [DB-format-version-break release](#db-format-version-break-release) |
| Migrate a regional MIG to zonal                      | [Regional-to-zonal migration](#regional-to-zonal-migration)   |
| Look up cache images, static IPs, daily cleanup      | [Reference](#reference)                                       |

## PR deploys

A PR deploy creates one zonal MIG `zebrad-${branch}-${network}-${zone-letter}` in `zfnd-dev-zebra`, bootstrapped from the latest matching cache image (preferring images from the same branch, falling back to `main`, then any branch). Lives until you reap it.

### Run one

```bash
gh workflow run zfnd-deploy-nodes-gcp.yml -R ZcashFoundation/zebra \
  --ref my-branch \
  -f network=Mainnet \
  -f zone=us-east1-b \
  -f environment=dev \
  -f need_cached_disk=true \
  -f cached_disk_type=tip
```

### Find yours

```bash
gcloud compute instance-groups managed list --project $P \
  --filter="labels.created_by=workflow_dispatch AND labels.github_ref=my-branch-slug"
```

### Spare from cleanup

Apply one of two labels:

- `keep_until=YYYY-MM-DD` — self-expiring; eligible for reaping after the date passes.
- `delete_protection=true` — indefinite; requires manual removal before reaping.

Apply to the instance and disk now if you need protection immediately (labels propagate to new instances on the next template swap):

```bash
MIG=zebrad-my-branch-mainnet-b
ZONE=us-east1-b
INSTANCE=$(gcloud compute instance-groups managed list-instances "$MIG" \
  --zone "$ZONE" --project $P --format='value(instance.basename())' | head -1)
gcloud compute instances add-labels "$INSTANCE" --zone "$ZONE" --project $P \
  --labels="keep_until=2026-05-01"
gcloud compute disks add-labels "zebrad-cache-my-branch-mainnet-b" \
  --zone "$ZONE" --project $P \
  --labels="keep_until=2026-05-01"
```

### Reap one

The stateful policy `auto-delete=on-permanent-instance-deletion` removes the disk with the MIG.

```bash
gcloud compute instance-groups managed delete "$MIG" --zone "$ZONE" --project $P --quiet
```

### Sweep expired

```bash
TODAY=$(date +%Y-%m-%d)
for line in $(gcloud compute instance-groups managed list --project $P \
    --filter="labels.created_by=workflow_dispatch" \
    --format='csv[no-heading](name,zone.basename(),labels.keep_until,labels.delete_protection)' \
    | awk -F, -v today="$TODAY" '$4 == "true" || $3 == "" { next } $3 < today { print $1 "|" $2 }'); do
  MIG="${line%|*}"
  ZONE="${line#*|}"
  echo "Reaping $MIG in $ZONE"
  gcloud compute instance-groups managed delete "$MIG" --zone "$ZONE" --project $P --quiet
done
```

Review the candidate list before piping into delete.

## Diagnose a stuck MIG

```bash
MIG=zebrad-main-mainnet-b
ZONE=us-east1-b

gcloud compute instance-groups managed describe "$MIG" --zone "$ZONE" --project $P \
  --format="value(currentActions,status.isStable,status.versionTarget.isReached)"
gcloud compute instance-groups managed list-instances "$MIG" --zone "$ZONE" --project $P \
  --format="table(NAME,STATUS,HEALTH_STATE,LAST_ERROR)"
gcloud compute instance-groups managed list-errors "$MIG" --zone "$ZONE" --project $P --limit=10
```

When `list-errors` reports `RESOURCE_IN_USE_BY_ANOTHER_RESOURCE` on a stateful disk, find the squatter:

```bash
DISK=zebrad-cache-main-mainnet-b
gcloud compute disks describe "$DISK" --zone "$ZONE" --project $P \
  --format="value(users.basename())"
```

The squatter is another MIG's instance. Reap that MIG (recipe above).

## Recover a corrupted cache disk

When Zebra crash-loops on a zonal disk, recover that one zone from the last good cache image. Sister zones keep serving.

```bash
NET=mainnet; ZONE=us-east1-b; ZONE_LETTER=b
MIG=zebrad-main-${NET}-${ZONE_LETTER}
DISK=zebrad-cache-main-${NET}-${ZONE_LETTER}
TS=$(date +%Y%m%d-%H%M)

# 1. Drain: preserve the disk via auto-delete=never, then scale to 0
INSTANCE=$(gcloud compute instance-groups managed list-instances "$MIG" \
  --zone "$ZONE" --project $P --format='value(name)' | head -1)
gcloud compute instance-groups managed instance-configs update "$MIG" \
  --zone "$ZONE" --project $P --instance "$INSTANCE" \
  --stateful-disk "device-name=${DISK},auto-delete=never"
gcloud compute instance-groups managed resize "$MIG" --size 0 --zone "$ZONE" --project $P

# 2. Snapshot the corrupted disk for forensics, then delete it
gcloud compute snapshots create "${DISK}-corrupted-${TS}" \
  --source-disk "$DISK" --source-disk-zone "$ZONE" --project $P
gcloud compute disks delete "$DISK" --zone "$ZONE" --project $P --quiet

# 3. Redeploy via workflow_dispatch for this single zone
gh workflow run zfnd-deploy-nodes-gcp.yml -R ZcashFoundation/zebra \
  -f network=Mainnet -f zone="$ZONE" -f environment=dev \
  -f need_cached_disk=true -f cached_disk_type=tip
```

For production: set `P=zfnd-prod-zebra`, `MIG=zebrad-${NET}-${ZONE_LETTER}`, `DISK=zebrad-cache-${NET}-${ZONE_LETTER}`. Production has no automatic cache image; restore from a recent operator-taken snapshot.

## DB-format-version-break release

When `zebra-state/src/constants.rs::DATABASE_FORMAT_VERSION` changes in a backwards-incompatible way, the in-place rolling swap fails because the new Zebra can't read the old RocksDB. Do it one zone at a time with snapshot-based handoff:

```bash
P=zfnd-prod-zebra; NET=mainnet; TS=$(date +%Y%m%d-%H%M)

for Z in b c d; do
  MIG=zebrad-${NET}-${Z}; DISK=zebrad-cache-${NET}-${Z}; ZONE=us-east1-${Z}

  # Snapshot before touching
  gcloud compute snapshots create "${DISK}-pre-major-${TS}" \
    --source-disk "$DISK" --source-disk-zone "$ZONE" --project $P

  # Drain, preserving disk
  INSTANCE=$(gcloud compute instance-groups managed list-instances "$MIG" \
    --zone "$ZONE" --project $P --format='value(name)' | head -1)
  gcloud compute instance-groups managed instance-configs update "$MIG" \
    --zone "$ZONE" --project $P --instance "$INSTANCE" \
    --stateful-disk "device-name=${DISK},auto-delete=never"
  gcloud compute instance-groups managed resize "$MIG" --size 0 --zone "$ZONE" --project $P
done

# Publish the release; the workflow's release path recreates each zonal MIG
# with the new template, which attaches the preserved disks.

# Rollback (only if the release fails): restore disks from snapshots, deploy previous tag
for Z in b c d; do
  DISK=zebrad-cache-${NET}-${Z}; ZONE=us-east1-${Z}
  gcloud compute disks delete "$DISK" --zone "$ZONE" --project $P --quiet
  gcloud compute disks create "$DISK" --zone "$ZONE" --project $P \
    --source-snapshot "${DISK}-pre-major-${TS}"
done
```

## Regional-to-zonal migration

One-shot procedure to convert an environment from the old regional architecture (one regional MIG per network holding three instances) to zonal (three zonal MIGs per network, one instance each). Run once per environment.

```bash
P=zfnd-prod-zebra   # or zfnd-dev-zebra
NET=mainnet
OLD_MIG=zebrad-${NET}    # for prod; use zebrad-main-${NET} for dev
OLD_DISK=zebrad-cache-${NET}    # for prod; use zebrad-cache-main-${NET} for dev
TS=$(date +%Y%m%d-%H%M)

# 1. Snapshot each zonal disk (three per network)
for Z in b c d; do
  gcloud compute snapshots create "${OLD_DISK}-${Z}-pre-zonal-${TS}" \
    --source-disk "$OLD_DISK" --source-disk-zone "us-east1-${Z}" --project $P
done

# 2. Delete the old regional MIG (disks auto-deleted; we have snapshots)
gcloud compute instance-groups managed delete "$OLD_MIG" \
  --region us-east1 --project $P --quiet

# 3. Create zonal disks from the snapshots with the new name scheme
#    Prod:    zebrad-cache-${NET}-${Z}
#    Staging: zebrad-cache-main-${NET}-${Z}
for Z in b c d; do
  NEW_DISK=zebrad-cache-${NET}-${Z}    # or zebrad-cache-main-${NET}-${Z}
  gcloud compute disks create "$NEW_DISK" \
    --zone "us-east1-${Z}" --project $P \
    --source-snapshot "${OLD_DISK}-${Z}-pre-zonal-${TS}" \
    --size 400 --type pd-balanced
done

# 4. Trigger workflow_dispatch for each (network, zone). The workflow sees
#    the zonal disks exist and attaches them; no fresh bootstrap from cache.
for Z in b c d; do
  gh workflow run zfnd-deploy-nodes-gcp.yml -R ZcashFoundation/zebra \
    -f network=Mainnet -f zone="us-east1-${Z}" \
    -f environment=$([ "$P" = "zfnd-prod-zebra" ] && echo prod || echo dev) \
    -f need_cached_disk=false
done
```

Repeat for Testnet. After all six (2 networks × 3 zones) zonal MIGs are HEALTHY, the migration for this environment is complete; retire the pre-zonal snapshots after a hold period.

## Reference

**Static IPs** are externally provisioned and map deterministically to zones:

- `us-east1-b` → `zebra-${network}` (primary)
- `us-east1-c` → `zebra-${network}-secondary`
- `us-east1-d` → `zebra-${network}-tertiary`

The workflow assigns them via per-instance configs for push and release deploys; workflow_dispatch deploys use ephemeral IPs. Reserve new ones manually with `gcloud compute addresses create` before adding capacity.

**Cache images** are produced by `zfnd-ci-integration-tests-gcp.yml`'s `create-state-image` job. Naming pattern: `{prefix}-{branch}-{sha}-v{state-version}-{network}-{tip|checkpoint}[-u]-{HHMMSS}`. One image per network seeds all three zones. Lookup priority: current branch, then `main`, then any branch; most recent first. List recent:

```bash
gcloud compute images list --project zfnd-dev-zebra \
  --filter="name~^zebrad-cache-.*-v27-mainnet-tip" \
  --sort-by=~creationTimestamp --limit=5
```

**Daily cleanup** (`zfnd-delete-gcp-resources.yml`) sweeps old instances, templates, disks, and cache images by age and name. It does not honor `keep_until` or `delete_protection` labels on PR deploys; use the [sweep recipe](#sweep-expired) for label-aware control. The twelve stable cache disks (`zebrad-cache-{mainnet,testnet}-{b,c,d}` in production, `zebrad-cache-main-{mainnet,testnet}-{b,c,d}` in staging) are never eligible because GCP refuses to delete attached disks; if any stable disk ever shows up unattached, that is itself an incident.

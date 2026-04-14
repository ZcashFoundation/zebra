# GCP Deployment Operations

Operational procedures for the GCP Continuous Delivery pipeline. Architectural rationale: [ADR 0006](../../../docs/decisions/devops/0006-gcp-deployment-naming.md). High-level model: [Continuous Delivery](continuous-delivery.md).

Two GCP projects (`zfnd-prod-zebra`, `zfnd-dev-zebra`) in `us-east1`, zones `b`, `c`, `d`. Every MIG, instance, and disk carries `environment`, `created_by`, `github_ref`, and `github_sha` labels. Use `created_by` as the kind discriminator: `release` (production), `push` (staging), `workflow_dispatch` (PR deploy).

The recipes below assume `gh` and `gcloud` are authenticated, and use these shell defaults when scoped to dev:

```bash
P=zfnd-dev-zebra; R=us-east1
```

For production, set `P=zfnd-prod-zebra` instead.

## Quick reference

| Goal                                                | Section                                                       |
| --------------------------------------------------- | ------------------------------------------------------------- |
| Smoke-test a PR branch / find / label / reap        | [PR deploys](#pr-deploys)                                     |
| Diagnose a deploy that does not converge            | [Diagnose a stuck MIG](#diagnose-a-stuck-mig)                 |
| Recover from a corrupted cache disk                 | [Recover a corrupted cache disk](#recover-a-corrupted-cache-disk) |
| Cut a release with a backwards-incompatible DB format | [DB-format-version-break release](#db-format-version-break-release) |
| Look up cache images, static IPs, daily cleanup     | [Reference](#reference)                                       |

## PR deploys

A PR deploy creates `zebrad-${branch}-${network}` in `zfnd-dev-zebra`, bootstrapped from the latest matching cache image (preferring images from the same branch, falling back to `main`, then any branch). It lives until you reap it.

### Run one

```bash
gh workflow run zfnd-deploy-nodes-gcp.yml -R ZcashFoundation/zebra \
  --ref my-branch -f network=Mainnet -f environment=dev \
  -f need_cached_disk=true -f cached_disk_type=tip
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

Apply to instances and disks now if you need protection immediately (otherwise the labels arrive on the next template swap):

```bash
MIG=zebrad-my-branch-mainnet
for inst in $(gcloud compute instance-groups managed list-instances "$MIG" \
  --region $R --project $P --format='value(name)'); do
  zone=$(gcloud compute instances describe "$inst" --project $P \
    --format='value(zone.basename())')
  gcloud compute instances add-labels "$inst" --zone "$zone" --project $P \
    --labels="keep_until=2026-05-01"
done
```

### Reap one

The stateful policy `auto-delete=on-permanent-instance-deletion` deletes the disks with the MIG.

```bash
gcloud compute instance-groups managed delete "$MIG" --region $R --project $P --quiet
```

### Sweep expired

```bash
TODAY=$(date +%Y-%m-%d)
for mig in $(gcloud compute instance-groups managed list --project $P \
    --filter="labels.created_by=workflow_dispatch" \
    --format='value(name,labels.keep_until,labels.delete_protection)' \
    | awk -v today="$TODAY" '$3 == "true" || $2 == "" { next } $2 < today { print $1 }'); do
  echo "Reaping $mig"
  gcloud compute instance-groups managed delete "$mig" --region $R --project $P --quiet
done
```

Review the candidate list before piping into delete.

## Diagnose a stuck MIG

```bash
MIG=zebrad-main-mainnet   # or the MIG you are debugging
gcloud compute instance-groups managed describe "$MIG" --region $R --project $P \
  --format="value(currentActions,status.isStable)"
gcloud compute instance-groups managed list-instances "$MIG" --region $R --project $P \
  --format="table(NAME,ZONE.basename(),STATUS,HEALTH_STATE,LAST_ERROR)"
gcloud compute instance-groups managed list-errors "$MIG" --region $R --project $P --limit=10
```

When `list-errors` reports `RESOURCE_IN_USE_BY_ANOTHER_RESOURCE` on a stateful disk, find the squatter:

```bash
DISK=zebrad-cache-main-mainnet
for z in b c d; do
  echo "us-east1-$z:"
  gcloud compute disks describe "$DISK" --zone "us-east1-$z" --project $P \
    --format="value(users.basename())" 2>/dev/null
done
```

The squatter is some other MIG's instance. Reap that MIG using the PR-deploy recipe above.

## Recover a corrupted cache disk

When Zebra crash-loops on a stateful disk (bad RocksDB write, hardware fault), recover from the last good cache image. Three steps: drain, snapshot-and-delete, redeploy.

```bash
NET=mainnet
MIG=zebrad-main-${NET}; DISK=zebrad-cache-main-${NET}; TS=$(date +%Y%m%d-%H%M)

# 1. Drain: preserve disks via auto-delete=never, then scale to 0
for inst in $(gcloud compute instance-groups managed list-instances "$MIG" \
  --region $R --project $P --format='value(name)'); do
  gcloud compute instance-groups managed instance-configs update "$MIG" \
    --region $R --project $P --instance "$inst" \
    --stateful-disk "device-name=${DISK},auto-delete=never"
done
gcloud compute instance-groups managed resize "$MIG" --size 0 --region $R --project $P

# 2. Snapshot the corrupted disks for forensics, then delete them
for z in b c d; do
  gcloud compute snapshots create "${DISK}-${z}-corrupted-${TS}" \
    --source-disk "$DISK" --source-disk-zone "us-east1-${z}" --project $P
  gcloud compute disks delete "$DISK" --zone "us-east1-${z}" --project $P --quiet
done

# 3. Redeploy; the workflow recreates disks from the latest cache image
gh workflow run zfnd-deploy-nodes-gcp.yml -R ZcashFoundation/zebra \
  -f network=Mainnet -f environment=dev -f need_cached_disk=true -f cached_disk_type=tip
```

For production: set `P=zfnd-prod-zebra`, `MIG=zebrad-${NET}`, `DISK=zebrad-cache-${NET}`. Production has no automatic cache image; restore from a recent operator-taken snapshot instead.

## DB-format-version-break release

When `zebra-state/src/constants.rs::DATABASE_FORMAT_VERSION` changes in a backwards-incompatible way, the in-place rolling swap fails: the new Zebra cannot read the old RocksDB. Use snapshot-based handoff.

```bash
P=zfnd-prod-zebra; NET=mainnet
MIG=zebrad-${NET}; DISK=zebrad-cache-${NET}; TS=$(date +%Y%m%d-%H%M)

# 1. Snapshot every zonal disk
for z in b c d; do
  gcloud compute snapshots create "${DISK}-${z}-pre-major-${TS}" \
    --source-disk "$DISK" --source-disk-zone "us-east1-${z}" --project $P
done

# 2. Drain the MIG, preserving disks (same drain block as the recovery recipe above)

# 3. Publish the release. The deploy workflow's release path creates a new MIG
#    that attaches the preserved disks. Watch for crash loops on the old DB.

# 4. Rollback (only if step 3 fails): restore disks from snapshots, deploy the previous tag
for z in b c d; do
  gcloud compute disks delete "$DISK" --zone "us-east1-${z}" --project $P --quiet
  gcloud compute disks create "$DISK" --zone "us-east1-${z}" --project $P \
    --source-snapshot "${DISK}-${z}-pre-major-${TS}"
done
```

## Reference

**Static IPs** are externally provisioned, named `zebra-${network}`, `zebra-${network}-secondary`, and `zebra-${network}-tertiary`. The workflow assigns them via per-instance configs; it does not create them. Reserve new ones manually with `gcloud compute addresses create` before adding capacity in a new region.

**Cache images** are produced by `zfnd-ci-integration-tests-gcp.yml`'s `create-state-image` job. Naming pattern: `{prefix}-{branch}-{sha}-v{state-version}-{network}-{tip|checkpoint}[-u]-{HHMMSS}`. Lookup priority in `gcp-get-cached-disks.sh`: current branch, then `main`, then any branch; most recent first. List recent:

```bash
gcloud compute images list --project zfnd-dev-zebra \
  --filter="name~^zebrad-cache-.*-v27-mainnet-tip" \
  --sort-by=~creationTimestamp --limit=5
```

**Daily cleanup** (`zfnd-delete-gcp-resources.yml`) sweeps old instances, templates, disks, and cache images by age and name. It does not honor `keep_until` or `delete_protection` labels; use the [PR-deploy sweep](#sweep-expired) for label-aware control. The four stable cache disks (`zebrad-cache-{mainnet,testnet}` in production, `zebrad-cache-main-{mainnet,testnet}` in staging) are never eligible because GCP refuses to delete attached disks; if a stable disk ever shows up unattached, that is itself an incident.

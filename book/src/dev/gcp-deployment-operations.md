# GCP Deployment Operations

Operational procedures for the GCP Continuous Delivery pipeline. Architectural rationale lives in [ADR 0006](../../../docs/decisions/devops/0006-gcp-deployment-lifecycle.md); the high-level model is in [Continuous Delivery](continuous-delivery.md).

## Quick reference

| Goal                                                   | Recipe                                                    |
| ------------------------------------------------------ | --------------------------------------------------------- |
| Deploy a PR branch to dev for smoke testing            | [Run a canary deploy](#run-a-canary-deploy)               |
| Find your canary deploys                               | [List your resources](#list-your-resources)               |
| Spare a canary from cleanup                            | [Label canary for retention](#label-canary-for-retention) |
| Tear down a canary you no longer need                  | [Reap a canary](#reap-a-canary)                           |
| Diagnose a stuck staging deploy                        | [Investigate a stuck MIG](#investigate-a-stuck-mig)       |
| Recover from a corrupted staging or prod cache disk    | [Recover a corrupted cache disk](#recover-a-corrupted-cache-disk) |
| Cut a release with a backwards-incompatible DB format  | [DB-format-version-break release](#db-format-version-break-release) |

## Concepts

The pipeline runs three lifecycle classes, each with its own GCP project, MIG name, and stateful disk. See the table in [Continuous Delivery](continuous-delivery.md). Every operation below is scoped to one class.

GCP projects:

- `zfnd-prod-zebra` (production releases)
- `zfnd-dev-zebra` (staging from main and per-branch canaries)

Region: `us-east1`, zones `b`, `c`, and `d`.

## Run a canary deploy

Use a canary to smoke-test a PR branch in the dev project. The canary creates a per-branch MIG and disk that you can iterate on, then tear down when you finish.

From the GitHub UI: Actions → Deploy Nodes to GCP → Run workflow → choose the branch, network, and environment.

From the CLI:

```bash
gh workflow run zfnd-deploy-nodes-gcp.yml -R ZcashFoundation/zebra \
  --ref my-branch \
  -f network=Mainnet \
  -f environment=dev \
  -f need_cached_disk=true \
  -f cached_disk_type=tip
```

The MIG is `zebrad-${branch-slug}-mainnet` in `zfnd-dev-zebra`, and the disk is `zebrad-cache-${branch-slug}-mainnet`. Bootstrap uses the latest matching cache image, preferring images from your branch, falling back to `main`, then any branch.

## List your resources

Find every MIG you created via dispatch:

```bash
gcloud compute instance-groups managed list \
  --project zfnd-dev-zebra \
  --filter="labels.created_by=workflow_dispatch AND labels.github_ref=my-branch-slug"
```

Find every disk and instance for a class or branch:

```bash
gcloud compute instances list --project zfnd-dev-zebra \
  --filter="labels.created_by=workflow_dispatch"

gcloud compute disks list --project zfnd-dev-zebra \
  --filter="labels.lifecycle_class=canary"
```

## Label canary for retention

The cleanup process is manual; labels are advisory but ritually respected. Operators recognize two label vocabularies:

- `keep_until=YYYY-MM-DD` protects a resource from cleanup before that date. Self-expiring; after the date passes, the resource becomes eligible for reaping.
- `delete_protection=true` protects indefinitely. Removing the label requires manual action before reaping.

Apply directly to instances and disks (the labels propagate to new instances on the next template swap, but apply now if you need protection immediately):

```bash
for inst in $(gcloud compute instance-groups managed list-instances zebrad-${branch}-mainnet \
  --region us-east1 --project zfnd-dev-zebra --format='value(name)'); do
  zone=$(gcloud compute instances describe "${inst}" --project zfnd-dev-zebra \
    --format='value(zone.basename())')
  gcloud compute instances add-labels "${inst}" --zone "${zone}" --project zfnd-dev-zebra \
    --labels="keep_until=2026-05-01"
done

for z in b c d; do
  gcloud compute disks add-labels "zebrad-cache-${branch}-mainnet" \
    --zone "us-east1-${z}" --project zfnd-dev-zebra \
    --labels="keep_until=2026-05-01"
done
```

## Reap a canary

Remove a canary MIG and its stateful disk. The stateful policy `auto-delete=on-permanent-instance-deletion` deletes the disk when the MIG is deleted.

```bash
P=zfnd-dev-zebra; R=us-east1; MIG=zebrad-my-branch-mainnet

# Inspect first: confirm ownership and protection status
gcloud compute instance-groups managed describe "${MIG}" --region $R --project $P
gcloud compute instances list --project $P --filter="name~^${MIG}-" \
  --format="table(name,labels.keep_until,labels.delete_protection)"

# Then delete
gcloud compute instance-groups managed delete "${MIG}" --region $R --project $P --quiet
```

To reap every canary whose `keep_until` has passed, use a manual sweep:

```bash
TODAY=$(date +%Y-%m-%d)
for mig in $(gcloud compute instance-groups managed list --project zfnd-dev-zebra \
    --filter="labels.lifecycle_class=canary" \
    --format='value(name,region.basename(),labels.keep_until,labels.delete_protection)' \
    | awk -v today="$TODAY" '
        $4 == "true" { next }                       # delete_protection=true: skip
        $3 == "" { next }                            # no keep_until: skip
        $3 < today { printf "%s\n", $1 }             # expired: candidate
      '); do
  echo "Reaping $mig"
  gcloud compute instance-groups managed delete "$mig" \
    --region us-east1 --project zfnd-dev-zebra --quiet
done
```

Adjust the policy if you also want to reap canaries with no `keep_until` after a default age (e.g., 14 days). Review the candidate list before piping into delete.

## Investigate a stuck MIG

Triage a deploy that does not converge:

```bash
P=zfnd-dev-zebra; R=us-east1; MIG=zebrad-main-mainnet

# Current actions and stability
gcloud compute instance-groups managed describe "${MIG}" --region $R --project $P \
  --format="value(currentActions,status.isStable)"

# Per-instance state and any explicit error
gcloud compute instance-groups managed list-instances "${MIG}" --region $R --project $P \
  --format="table(NAME,ZONE.basename(),STATUS,HEALTH_STATE,LAST_ERROR)"

# Recent operation errors (most useful for RESOURCE_IN_USE_BY_ANOTHER_RESOURCE)
gcloud compute instance-groups managed list-errors "${MIG}" --region $R --project $P --limit=10
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

The squatter is some other MIG's instance. Identify the owning MIG and reap it (see [Reap a canary](#reap-a-canary)).

## Recover a corrupted cache disk

If Zebra crash-loops on a stateful disk because of a bad RocksDB write or hardware fault, recover from the last good cache image.

```bash
P=zfnd-dev-zebra; R=us-east1; NET=mainnet
MIG=zebrad-main-${NET}; DISK=zebrad-cache-main-${NET}

# 1. Drain the MIG, preserving disks via auto-delete=never, then scale to 0
for inst in $(gcloud compute instance-groups managed list-instances "${MIG}" \
  --region $R --project $P --format='value(name)'); do
  gcloud compute instance-groups managed instance-configs update "${MIG}" \
    --region $R --project $P --instance "${inst}" \
    --stateful-disk "device-name=${DISK},auto-delete=never"
done
gcloud compute instance-groups managed resize "${MIG}" --size 0 --region $R --project $P

# 2. Snapshot the corrupted disks for forensics, then delete them
TS=$(date +%Y%m%d-%H%M)
for z in b c d; do
  gcloud compute snapshots create "${DISK}-${z}-corrupted-${TS}" \
    --source-disk "${DISK}" --source-disk-zone "us-east1-${z}" --project $P
  gcloud compute disks delete "${DISK}" --zone "us-east1-${z}" --project $P --quiet
done

# 3. Re-deploy via workflow_dispatch; the workflow recreates disks from the latest cache image
gh workflow run zfnd-deploy-nodes-gcp.yml -R ZcashFoundation/zebra \
  -f network=Mainnet -f environment=dev -f need_cached_disk=true -f cached_disk_type=tip
```

For prod, follow the same sequence with `MIG=zebrad-mainnet`, `DISK=zebrad-cache-mainnet`, and `environment=prod` in `zfnd-prod-zebra`. Prod has no automatic cache image, so for a known-good prod state, use a recent operator-taken snapshot.

## DB-format-version-break release

A release that changes `zebra-state/src/constants.rs::DATABASE_FORMAT_VERSION` in a backwards-incompatible way cannot use the in-place rolling template swap, because the new Zebra version cannot read the old RocksDB. Use this snapshot-based handoff instead:

```bash
P=zfnd-prod-zebra; R=us-east1; NET=mainnet
MIG=zebrad-${NET}; DISK=zebrad-cache-${NET}; TS=$(date +%Y%m%d-%H%M)

# 1. Snapshot every zonal disk
for z in b c d; do
  gcloud compute snapshots create "${DISK}-${z}-pre-major-${TS}" \
    --source-disk "${DISK}" --source-disk-zone "us-east1-${z}" --project $P
done

# 2. Drain the MIG, preserving disks
for inst in $(gcloud compute instance-groups managed list-instances "${MIG}" \
  --region $R --project $P --format='value(name)'); do
  gcloud compute instance-groups managed instance-configs update "${MIG}" \
    --region $R --project $P --instance "${inst}" \
    --stateful-disk "device-name=${DISK},auto-delete=never"
done
gcloud compute instance-groups managed resize "${MIG}" --size 0 --region $R --project $P

# 3. Wait for the release to publish; the deploy workflow's release path creates
#    the new MIG with the new template, which attaches the preserved disks.
#    If the new Zebra crash-loops on the old DB, fall back to step 4.

# 4. (Rollback only) Recreate disks from snapshots and restore the old MIG template
for z in b c d; do
  gcloud compute disks delete "${DISK}" --zone "us-east1-${z}" --project $P --quiet
  gcloud compute disks create "${DISK}" --zone "us-east1-${z}" --project $P \
    --source-snapshot "${DISK}-${z}-pre-major-${TS}"
done
# Then deploy the previous (working) release tag from the GitHub UI.
```

## Static IPs

Static IPs are externally provisioned and named `zebra-${network}`, `zebra-${network}-secondary`, and `zebra-${network}-tertiary`. The workflow's per-instance config step assigns them to MIG instances; the workflow does not create them. Reserve them manually via `gcloud compute addresses create` before adding capacity in a new region.

## Cache images

Cache images are the `image=` source for fresh disk creation. The integration test workflow (`zfnd-ci-integration-tests-gcp.yml`'s `create-state-image` step) produces them with this naming pattern:

```
{disk_prefix}-{branch}-{sha}-v{state_version}-{network}-{tip|checkpoint}[-u]-{HHMMSS}
```

Lookup priority in `gcp-get-cached-disks.sh`: current branch, then `main`, then any branch. Most recent first.

When integration tests have not produced a recent enough image, a fresh deploy starts from genesis (multi-day sync). Verify the image inventory before triggering a clean-slate deploy:

```bash
gcloud compute images list --project zfnd-dev-zebra \
  --filter="name~^zebrad-cache-.*-v27-mainnet-tip" \
  --sort-by=~creationTimestamp --limit=5 \
  --format="table(name,creationTimestamp.date('%Y-%m-%d'))"
```

## Cleanup

`zfnd-delete-gcp-resources.yml` runs daily and sweeps old instances, templates, disks, and cache images. The disk sweeper relies on age and name; it does not honor `keep_until` or `delete_protection` labels on canaries. Use the [Reap a canary](#reap-a-canary) recipe for label-aware control.

The four stable cache disks (`zebrad-cache-{mainnet,testnet}` in prod, `zebrad-cache-main-{mainnet,testnet}` in staging) are attached to running MIGs at all times. They are not eligible for the daily sweep because GCP refuses to delete an attached disk; if a stable disk ever shows up unattached, that itself is an incident.

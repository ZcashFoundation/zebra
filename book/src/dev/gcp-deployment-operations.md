# GCP deployment operations

Use the [continuous-delivery topology](continuous-delivery.md#zonal-instance-groups)
to select the project, zone, managed instance group (MIG), and state disk.
Authenticate `gh` and `gcloud` with authorized repository and project
accounts. Set the target explicitly; these values select one stage
Mainnet node:

```sh
P=zfnd-dev-zebra
ZONE=us-east1-b
MIG=zebrad-main-mainnet-b
DISK=zebrad-cache-main-mainnet-b
```

For next, use the `next-` prefix instead of `main-`. For production, set
`P=zfnd-prod-zebra` and omit the prefix:
`MIG=zebrad-mainnet-b`, `DISK=zebrad-cache-mainnet-b`.
Use `--zone` for these MIGs, not `--region`.

Instance templates and new disks receive `app`, `environment`, `network`,
`zone`, `created_by`, `github_ref`, and `github_sha` labels; instances
inherit template labels. Existing disk labels are not refreshed. Find MIGs
by name: they do not receive these labels.

## Deploy success has two stages

Each [deployment job](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zfnd-deploy-prebuilt-nodes-gcp.yml)
reports:

1. **Template rollout:** `wait-until --version-target-reached --timeout=600`.
2. **Group stability:** `wait-until --stable --timeout=5400`, only with
   `verify=true`.

The [per-MIG lock](continuous-delivery.md#update-mechanics) covers both stages.
Pushes and releases enable the second stage; manual dispatches do not.
Check `instanceHealth` and container logs even after the group is stable.

The workflow creates an HTTP health check on port 8080 at `/healthy`, with
a 60-second interval, a 10-second timeout, three failed probes to become
unhealthy, and two successful probes to become healthy. New MIGs have a
3,600-second autohealing initialization delay. The node requires at least
one recently live peer through `ZEBRA_HEALTH__MIN_CONNECTED_PEERS=1`.
Check sync progress separately before serving clients:
[`/healthy` checks liveness, not sync readiness](../user/health.md).

Rollout or stability failures print instance state and recent container
logs; `failure-issue` opens or updates an issue labeled `ci-fail/release`.
Investigate the failing stage.

Automatic deployments [do not serialize zones](continuous-delivery.md#update-mechanics).
Use one-zone manual deployments for a controlled migration.

## PR deploys

### Run one

Dispatch from a repository-owned feature branch to create or update one
branch-specific MIG:

```sh
gh workflow run zfnd-deploy-nodes-gcp.yml \
  --repo ZcashFoundation/zebra --ref my-branch \
  -f network=Mainnet -f zone=us-east1-b -f environment=dev \
  -f need_cached_disk=true -f cached_disk_type=tip
```

The `network` input accepts `Mainnet` or `Testnet`. The `environment` input
accepts `dev`, `next`, or `prod`; there is no `stage` choice. To update stage
manually, select `dev` from the `main` ref. That targets stage resource
names but uses `dev` environment variables and Sentry labeling.

Selecting `next` or `prod` targets the stable environment regardless of
the selected source ref. Selecting `dev` from `next` also targets next's
resource names. Do not use these combinations for an isolated PR test.

### Find yours

Resource names use the URL-safe ref slug produced by
`rlespinasse/github-slug-action`. For the example branch:

```sh
gcloud compute instance-groups managed list --project "$P" \
  --filter='name=zebrad-my-branch-mainnet-b'
gcloud compute instances list --project "$P" \
  --filter='labels.created_by=workflow_dispatch AND labels.github_ref=my-branch'
```

See [branch-deployment networking](continuous-delivery.md#state-and-addresses)
for external IP and outbound connectivity behavior.

### Reap one

Confirm that the MIG is a disposable branch deployment, not stage, next,
or production. The workflow's disk policy is
`auto-delete=on-permanent-instance-deletion`: deleting the MIG or reducing
its size can delete its state disk. Snapshot state that must survive first.

```sh
MIG=zebrad-my-branch-mainnet-b
ZONE=us-east1-b
gcloud compute instance-groups managed delete "$MIG" \
  --zone "$ZONE" --project "$P"
```

Keep the confirmation prompt. Remove a template only after checking that no
MIG references it. [Daily cleanup](#daily-cleanup) does not reap branch MIGs;
`keep_until` and `delete_protection` labels do not protect resources.

## Diagnose a stuck MIG

Inspect template convergence, instance health, and creation errors:

```sh
gcloud compute instance-groups managed describe "$MIG" \
  --zone "$ZONE" --project "$P" \
  --format='yaml(targetSize,currentActions,status,versions,statefulPolicy)'
gcloud compute instance-groups managed list-instances "$MIG" \
  --zone "$ZONE" --project "$P" \
  --format='table(instance.basename(),instanceStatus,currentAction,instanceHealth[0].detailedHealthState,lastAttempt.errors.errors[0].message)'
gcloud compute instance-groups managed list-errors "$MIG" \
  --zone "$ZONE" --project "$P" --limit=10
```

For a disk attachment conflict, inspect the disk's users:

```sh
gcloud compute disks describe "$DISK" --zone "$ZONE" --project "$P" \
  --format='yaml(name,sizeGb,users)'
```

Inspect each reported instance's `created-by` metadata to identify its MIG.
Do not delete an instance or another MIG merely because it holds the disk.
Confirm which deployment owns the state, stop competing deployments, and
preserve any required disk before removing an obsolete owner. A read-write
state disk must not be shared between independent MIGs.

Read recent container logs for the selected instance:

```sh
INSTANCE=$(gcloud compute instance-groups managed list-instances "$MIG" \
  --zone "$ZONE" --project "$P" --format='value(instance.basename())')
INSTANCE_ID=$(gcloud compute instances describe "$INSTANCE" \
  --zone "$ZONE" --project "$P" --format='value(id)')
gcloud logging read \
  "logName=\"projects/${P}/logs/cos_containers\" AND resource.labels.instance_id=\"${INSTANCE_ID}\"" \
  --project "$P" --freshness=2h --limit=100 \
  --format='value(jsonPayload.message)'
```

These commands require exactly one instance. If there are none, inspect
creation errors first. An ordinary deployment updates a size-zero MIG's
template without resizing it.

## Recover a corrupted cache disk

Recover one zone at a time. Stop concurrent deployments to the same MIG
before changing its disk. Select a known-good snapshot or compatible cache
image before deleting anything, and retain a snapshot of the failed disk
for diagnosis.

### Detach and preserve the existing disk

Change the group's disk policy to preserve the disk on permanent deletion:

```sh
gcloud compute instance-groups managed update "$MIG" \
  --zone "$ZONE" --project "$P" \
  --stateful-disk="device-name=${DISK},auto-delete=never"
gcloud compute instance-groups managed describe "$MIG" \
  --zone "$ZONE" --project "$P" --format='yaml(statefulPolicy)'
gcloud compute instance-groups managed instance-configs list "$MIG" \
  --zone "$ZONE" --project "$P" --format=json
```

Confirm the effective [disk policy](https://cloud.google.com/compute/docs/instance-groups/configuring-stateful-disks-in-migs)
is `NEVER`; change any conflicting per-instance auto-delete rule to `never`
before proceeding. Preserve reserved external addresses; their workflow
policy already uses `auto-delete=never`.

Delete only the selected MIG, then confirm that the disk has no users:

```sh
gcloud compute instance-groups managed delete "$MIG" \
  --zone "$ZONE" --project "$P"
gcloud compute disks describe "$DISK" --zone "$ZONE" --project "$P" \
  --format='yaml(name,users)'
```

Do not continue while the disk is attached. Snapshot the detached disk:

```sh
SNAPSHOT="${DISK}-recovery-$(date -u +%Y%m%d-%H%M%S)"
gcloud compute snapshots create "$SNAPSHOT" --project "$P" \
  --source-disk "$DISK" --source-disk-zone "$ZONE"
gcloud compute snapshots describe "$SNAPSHOT" --project "$P" \
  --format='yaml(status,sourceDisk,creationTimestamp)'
```

Require `READY` before discarding the original disk. This snapshot preserves
the failed state; it is not the known-good restore source.

### Restore and redeploy

Set `RESTORE_SNAPSHOT` to the verified, known-good snapshot in the selected
project. Check its source disk, network, and database compatibility:

```sh
RESTORE_SNAPSHOT=known-good-snapshot
gcloud compute snapshots describe "$RESTORE_SNAPSHOT" --project "$P" \
  --format='yaml(status,sourceDisk,diskSizeGb,creationTimestamp)'
```

After confirming it is `READY`, replace the disk under the same name:

```sh
gcloud compute disks delete "$DISK" --zone "$ZONE" --project "$P"
gcloud compute disks create "$DISK" --zone "$ZONE" --project "$P" \
  --source-snapshot "$RESTORE_SNAPSHOT" --type=pd-balanced
```

Alternatively, [create the disk from a verified cache image](#bootstrap-a-disk-without-a-cache-image).
Dispatch the intended source ref, network, zone, and environment with
`need_cached_disk=false`. For the stage defaults above:

```sh
gh workflow run zfnd-deploy-nodes-gcp.yml \
  --repo ZcashFoundation/zebra --ref main \
  -f network=Mainnet -f zone="$ZONE" -f environment=dev \
  -f need_cached_disk=false
```

For next, use `--ref next -f environment=next`. For production, use the
approved release tag with `-f environment=prod`. The absent MIG is recreated
at size one, its stateful disk policy is restored, and the workflow attempts
to bind its reserved address. Verify health and the address before
recovering another zone.

## Bootstrap a disk without a cache image

Deployment [cache policy](continuous-delivery.md#state-and-addresses) permits
missing images, not missing disks. Production disks must exist before a
release-triggered deployment.

To seed a missing disk manually, select a verified image for the same
network and a compatible database version. Set `IMAGE_PROJECT` to the
project containing it and `IMAGE` to its exact name:

```sh
IMAGE_PROJECT=zfnd-dev-zebra
IMAGE=verified-cache-image
gcloud compute images describe "$IMAGE" --project "$IMAGE_PROJECT" \
  --format='yaml(name,status,diskSizeGb,labels,description)'
```

Require `READY` and sufficient disk capacity, then create the disk:

```sh
gcloud compute disks create "$DISK" --zone "$ZONE" --project "$P" \
  --image="$IMAGE" --image-project="$IMAGE_PROJECT" \
  --size=400GB --type=pd-balanced
```

Increase the size if the source image requires more than 400 GB. A snapshot
can be used instead with `--source-snapshot`. Dispatch with
`need_cached_disk=false` after creating the disk.

Check [state compatibility and rollback requirements](../user/upgrading.md)
before selecting an older image; it does not necessarily avoid a full resync.

## DB-format-version-break release

A database major-version change does not by itself require a fresh disk.
Use the [upgrade and rollback requirements](../user/upgrading.md), not the
version number, to plan recovery.

Before a format-changing rollout:

1. Check source and destination [state compatibility](../user/upgrading.md).
2. Take independent, consistent snapshots before the new binary writes to
   each disk. Stop the writer and any direct database readers while taking
   the rollback copy. Use the preservation procedure above when removing a
   MIG to detach its disk.
3. Deploy one network/zone manually, then verify its application health and
   logs before proceeding. Publishing a release starts the automatic
   six-cell deployment; it does not provide this one-zone ordering.
4. Retain the snapshots until rollback to the previous binary is no longer
   required.

For rollback, use [disk recovery](#recover-a-corrupted-cache-disk) with a
pre-upgrade snapshot and dispatch the previous approved tag to the same
production destination. Do not start the old binary against a disk whose
format it cannot read. Preserve the failed upgraded disk for diagnosis
before replacing it. Verify each restored node before moving to another
zone.

## Regional-to-zonal migration

A regional MIG cannot be renamed into three zonal MIGs. For an existing
regional deployment, schedule the cutover explicitly; the workflow does
not migrate it automatically.

1. Inventory the regional MIG, its three instances, zonal disks, and
   reserved addresses. Record the current template and exact source ref.
2. Stop writers and preserve every state disk. Set each effective stateful
   disk policy to `never` before deleting the regional MIG; use `--region`
   for the old group, not the zonal commands above.
3. Confirm all disks are detached. Snapshot each zone's disk and require
   `READY` for all snapshots before replacing or deleting old resources.
4. Create a disk in each destination zone from the corresponding snapshot,
   using the [current naming scheme](continuous-delivery.md#zonal-instance-groups).
5. Dispatch the selected source ref once per network and zone with
   `need_cached_disk=false`. Verify each new MIG's health and reserved IP.
6. Remove obsolete disks and templates only after the new deployment is
   operational and the rollback copies are retained.

Repeat for both networks. Do not run an unattended loop that deletes all
zones before verifying the first replacement.

## Reference

### Static IPs

| Zone | Stage and production address name | Next address name |
| --- | --- | --- |
| `us-east1-b` | `zebra-${network}` | `zebra-next-${network}` |
| `us-east1-c` | `zebra-${network}-secondary` | `zebra-next-${network}-secondary` |
| `us-east1-d` | `zebra-${network}-tertiary` | `zebra-next-${network}-tertiary` |

Reserve addresses before deployment. Stage and production reuse names in
different projects; next has separate names in the development project.

The workflow resolves the address for stable destinations and advertises
it through `ZEBRA_NETWORK__EXTERNAL_ADDR`, but assigns it only during fresh
MIG creation. A missing reservation produces a warning and skips binding.
An existing MIG does not get its binding repaired by rerunning deployment.

Inspect the instance's actual `networkInterfaces` and its per-instance
configuration before repairing it. For a missing per-instance
configuration, use the same operation as the workflow:

```sh
IP_NAME=zebra-mainnet
IP_ADDRESS=$(gcloud compute addresses describe "$IP_NAME" \
  --region=us-east1 --project "$P" --format='value(address)')
gcloud compute instance-groups managed instance-configs create "$MIG" \
  --instance="$INSTANCE" --zone="$ZONE" --project "$P" \
  --stateful-external-ip="address=${IP_ADDRESS},interface-name=nic0,auto-delete=never"
```

Require a nonempty address first. Select the correct name for the
environment and zone. If the configuration already exists, update it
rather than creating another. The command applies the configuration to
the instance by default. Verify that its actual external address matches
the advertised address after the update; successful submission alone does
not establish that the binding is live.

### Cache images

The integration workflow's `create-state-image` job produces names of the
form:

```text
{prefix}-{ref-slug}-{sha}-v{major}-{network}-{tip|checkpoint}[-u]-{HHMMSS}
```

The prefix is `zebrad-cache` or `lwd-cache`. The ref slug is truncated to 12
characters, the SHA to seven, and the time is UTC. The database major comes
from the running test's logs, not merely from the source constant.

The [lookup script](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/scripts/gcp-get-cached-disks.sh)
requires a `READY` image matching purpose, database major, network, and
state kind. It tries the current branch's truncated ref slug followed by a
commit SHA, then `main`, then any branch, selecting the newest match in each
query. Branch slugs can collide after truncation; selection does not require
the current commit. Inspect the chosen image in the job output.

The `allow_missing` input permits an empty selected name, not a failed
image-list query. List recent v29 Mainnet tip images with:

```sh
gcloud compute images list --project zfnd-dev-zebra \
  --filter='status=READY AND name~^zebrad-cache-.*-v29-mainnet-tip' \
  --sort-by='~creationTimestamp' --limit=5
```

### Sentry releases

Successful `stage`, `next`, and `prod` deployments run `sentry-release-name`.
It reads the node's release name from the exact image with `zebrad --version`,
using an environment, `id-token: write`, and `GCP_ARTIFACTS_SA` to pull from GAR.

The separate `sentry-release` job has no GitHub environment and only
`deployments: read` permission, keeping GCP credentials out of the Sentry
action's container. Using optional repository secret `SENTRY_AUTH_TOKEN`, it
records the release and deployment environment in organization `zfnd`,
project `zebrad`.

The commit range ends at the deployed source SHA. Among the latest 50 GitHub
deployments in the same environment, the job finds a different SHA whose
latest status is `success` and passes it as `previous_commit`. Without a
match, it supplies no previous commit; it never uses another environment's
newest Sentry release.

Without `SENTRY_AUTH_TOKEN`, the recording job skips its steps; the image
version reader still runs. Deployments selecting `dev` do not enter this
recording path, even when their ref targets stage or next resource names.

Zebra reads [CI and Git metadata](https://github.com/ZcashFoundation/zebra/blob/main/zebrad/src/sentry.rs#L78-L130)
from the container's runtime environment. Pass these values at deployment;
do not bake environment-specific metadata into shared images. Sentry events
carry tags and a `ci` context; Logs carry the same tags as attributes and
prefix the context fields with `ci.`.

| Runtime variable | Sentry metadata |
| --- | --- |
| `GITHUB_ACTIONS` | Sets `ci.provider=github-actions` when equal to `true`, ignoring case. |
| `GITHUB_EVENT_NAME` | `deploy.trigger` tag. |
| `CI_PR_NUMBER`, `CI_TEST_ID` | `pr.number` and `test.id` tags. |
| `GITHUB_RUN_ID`, `GITHUB_RUN_ATTEMPT` | `run_id` and `run_attempt` in the CI context. |
| `GITHUB_WORKFLOW`, `GITHUB_JOB` | `workflow` and `job` in the CI context. |
| `GITHUB_SHA` | `git.sha` tag; otherwise uses the build-time `SHORT_SHA`, then `VERGEN_GIT_SHA`, when available. |
| `GITHUB_REF_POINT_SLUG_URL`, `GITHUB_HEAD_REF_SLUG_URL`, `GITHUB_REF_NAME_SLUG_URL` | First nonempty value becomes the `git.ref` tag, in this order. Raw `GITHUB_REF_NAME` is not used. |

Runtime values are trimmed; empty values are ignored. The
[node deployment](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zfnd-deploy-prebuilt-nodes-gcp.yml#L435)
passes its source SHA as `GITHUB_SHA` and its selected environment as
`SENTRY_ENVIRONMENT`. The
[integration-test deployment](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zfnd-deploy-integration-tests-gcp.yml#L336-L338)
uses `SENTRY_ENVIRONMENT=ci` for every trigger, keeping test runs out of the
fleet's release-health environment.

### Daily cleanup

The [cleanup workflow](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zfnd-delete-gcp-resources.yml)
runs daily at 07:00 UTC, or manually, with the `dev` environment. Its
scripts use age and name filters, not `keep_until` or `delete_protection`:

| Resource | Selection and retention |
| --- | --- |
| Instances | Names ending in `-[0-9a-f]{7,}`; three-day age cutoff, or six days with `long-test=true` |
| Instance templates | The same name suffix; two-day age cutoff |
| Disks | Unattached, older than the two-day cutoff, and either the suffix above or a `zebrad-` prefix excluding `zebrad-cache` |
| Cache images | Preserve all newer than the two-day cutoff and the newest two older images per network/state-kind category |

Cutoffs are dates, not exact elapsed-hour deadlines. Decimal GitHub run IDs
match the hexadecimal-looking suffix filter. Current stable cache disk
names end in a zone letter and begin with `zebrad-cache-`, so neither disk
selector matches them, even when detached. Current CD template names also
end in a zone letter and are outside the template selector. Do not assume
the cleanup workflow reaps branch MIGs or their templates.

Image retention is not partitioned by branch or database major. Preserve
required rollback snapshots independently of CI cache-image retention.

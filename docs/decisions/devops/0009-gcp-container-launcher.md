---
status: accepted
date: 2026-11-13
builds-on: [GCP Deployment Topology](0006-gcp-deployment-naming.md)
story: Google deprecated the container startup agent (konlet) that started every Zebra container on Compute Engine, tracked in https://github.com/ZcashFoundation/zebra/issues/11489
---

# Start GCP containers from a cloud-init systemd unit instead of konlet

## Context and Problem Statement

Zebra's integration-test VMs and its stage, next and prod node groups ran their container through the container startup agent (konlet): `gcloud compute instances create-with-container` and `instance-templates create-with-container` write the `gce-container-declaration` metadata key, and konlet on Container-Optimized OS (COS) reads it at boot. Google deprecated konlet. Creating VMs with that metadata is unsupported from 2026-07-31, and support ends on 2027-07-31. Newer COS releases drop konlet, and our templates use the moving `cos-stable` image family, so the deployments would stop starting containers whenever `cos-stable` moved to such a release, not on a date we chose.

## Priorities & Constraints

- Keep COS, the stateful zonal MIG topology from [ADR 0006](0006-gcp-deployment-naming.md), and its rolling-update contract.
- A node's chain state on its stateful disk must survive the switch, reboots and rolling updates.
- The steps that run after the container must keep working: they read its exit status and its `docker logs` output over SSH, and read its output from Cloud Logging.
- One mechanism for test VMs and node templates.

## Considered Options

- Option 1: A startup script that runs `docker run`.
- Option 2: A cloud-init document that installs systemd units.
- Option 3: Move to GKE or Cloud Run.

### Pros and Cons of the Options

#### Option 1: Startup script

- Good, because it is the smallest change.
- Bad, because restart, stop timeout, ordering after the data disk mount, and boot-time start all have to be hand-written in shell.

#### Option 2: cloud-init systemd units

- Good, because systemd provides restart policy, stop timeout and mount ordering, and Google documents cloud-init as a replacement.
- Good, because a unit can wait for zebrad to flush RocksDB on stop. konlet inherited docker's 10s stop timeout.
- Bad, because konlet behaviours the workflows relied on must be reproduced explicitly (below).

#### Option 3: GKE or Cloud Run

- Bad, because a single-writer stateful disk per node does not fit Cloud Run, and GKE replaces the topology ADR 0006 settled on for a deprecation that only affects how the container starts.

## Decision Outcome

Chosen option: Option 2. `.github/workflows/scripts/gcp-container-cloud-init.sh` prints one cloud-init document, configured through environment variables, and both the integration-test workflow and the node template step use it. The integration tests run with `Restart=no`, and the nodes run with `Restart=always`.

The document reproduces these konlet behaviours, each of which a workflow step depended on:

| konlet | cloud-init unit | What depends on it |
| --- | --- | --- |
| formats a blank data disk | a oneshot unit runs `blkid \|\| mkfs.ext4` | new disks; a disk that already has a filesystem is never reformatted |
| `--container-restart-policy=never` keeps the exited container | `Restart=no`, and no `docker run --rm` | test steps that read the exited container's status and logs |
| `--container-tty` | `docker run --tty` | the TTY merges zebrad's stderr into the stdout that log-parsing steps read |
| container named `klt-<instance>` | a fixed `CONTAINER_NAME` | the step that finds the test container |
| default restart policy `always` | `Restart=always` with `RestartSec=10` | nodes; systemd stops restarting a unit that starts more than 5 times in 10s, so a 100ms delay would let a crash loop end the node |

Two choices avoid silent failures:

- The host mount point, `/mnt/disks/zebracache`, has no hyphen. A hyphen needs a `\x2d` escape in the mount unit's name, and that backslash does not survive cloud-init's `runcmd`. The mount then fails to enable while the container still starts on the boot disk. The container unit also `Requires=` the mount, so a failed mount stops the container instead.
- The node template's identity hash includes a digest of the generator script, and the node and integration-test workflows list the script in their `paths` filters. Without the digest, redeploying unchanged configuration would reuse the existing konlet template. Without the filters, a change to the generator alone would neither redeploy the nodes nor run the tests on `main`.

### Expected Consequences

- Image pulls use no registry credentials. Both Artifact Registry repositories grant `allUsers` the reader role. If either becomes private, the units need `docker-credential-gcr` configured first.
- The first deploy after this change replaces every node's template. The stage and prod node groups keep no per-instance `gce-container-declaration`, so the rolling update needs no cleanup step.
- Zebra no longer depends on konlet. Node groups managed outside this repository may still use it.

## More Information

Validated on 2026-11-13 in `zfnd-dev-zebra`, on temporary resources:

- A size-1 stateful MIG started on a konlet template, created a database, then rolled to a cloud-init template with the workflow's `--replacement-method=recreate --max-surge=0 --max-unavailable=1`. The recreated instance had no `gce-container-declaration`, ran one container, and zebrad reopened the database konlet's container had created.
- On COS 133 (`cos-beta`), the units started at boot and again after a reset without reformatting the disk, and the container's output reached the `cos_containers` log that the deploy failure diagnostics read. konlet also still ran on COS 133 that day.
- Killing the node container restarted it after 10s, three times in a row.

- [Prepare for the shutdown of the container startup agent](https://docs.cloud.google.com/compute/docs/containers/prepare-for-container-agent-shutdown)
- [Migrate containers that were deployed on VMs during VM creation](https://docs.cloud.google.com/compute/docs/containers/migrate-containers)

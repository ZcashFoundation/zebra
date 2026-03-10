# Zebra Continuous Delivery

Zebra has an extension of its continuous integration since it automatically deploys all
code changes to a testing and/or pre-production environment after each PR gets merged
into the `main` branch, and on each Zebra `release`.

## Triggers

The Continuous delivery pipeline is triggered when:

* A PR is merged to `main` (technically, a `push` event)
* A new release is published in GitHub

## Deployments

On each trigger Zebra is deployed using the branch or version references as part of
the deployment naming convention. Deployments are made using [Managed Instance Groups (MIGs)](https://cloud.google.com/compute/docs/instance-groups#managed_instance_groups)
from Google Cloud Platform with, 2 nodes in the us-central1 region.

**Note**: These *MIGs* are always replaced when PRs are merged to the `main` branch and
when a release is published. If a new major version is released, a new *MIG* is also 
created, keeping the previous major version running until it's no longer needed.

A single instance can also be deployed, on an on-demand basis, if required, when a
long-lived instance, with specific changes, is needed to be tested in the Mainnet with
the same infrastructure used for CI & CD.

Further validations of the actual process can be done on our continuous delivery [workflow file](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zfnd-deploy-nodes-gcp.yml).

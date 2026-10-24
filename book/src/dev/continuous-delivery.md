# Zebra continuous delivery

The [deployment wrapper](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zfnd-deploy-nodes-gcp.yml)
builds a container image and passes its immutable Google Artifact Registry
(GAR) identity, `repository@sha256:digest`, to the
[reusable deployment workflow](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zfnd-deploy-prebuilt-nodes-gcp.yml).
The source commit is a separate, full 40-character SHA input.

See the [workflow inventory](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/README.md),
[deployment runbook](gcp-deployment-operations.md), and
[naming rationale](https://github.com/ZcashFoundation/zebra/blob/main/docs/decisions/devops/0006-gcp-deployment-naming.md).

## Triggers and environments

| Trigger | GitHub environment | Deployment |
| --- | --- | --- |
| Matching push to `main` | `stage` | Mainnet and Testnet in all three zones |
| Matching push to `next` | `next` | Mainnet and Testnet in all three zones |
| `release: released` with a tag starting with `v` | `prod` | Mainnet and Testnet in all three zones |
| Manual dispatch | Selected `dev`, `next`, or `prod` | One selected network and zone |
| Matching repository-owned PR | `dev` | Image build only; no node deployment |

Push and PR path filters include Rust source, text files, Cargo manifests
and lockfiles, `.cargo/config.toml`, Clippy configuration, `docker/**`,
`.dockerignore`, and the three build/deployment workflow files listed in
the wrapper. These filters are independent of `.github/path-filters.yml`.
A documentation-only push does not deploy nodes. Fork PRs skip the
wrapper's image build.

The `released` activity covers stable publication and promotion of a
prerelease to a stable release. Prerelease publication does not trigger this
workflow. Manual dispatch still uses the selected environment. Docker Hub
publication belongs to release automation, not this deployment wrapper.
See the [release process](release-process.md) before publishing a release
or dispatching a production deployment.

Deployment jobs use environment-scoped machine type, subnet, region, logging,
and Sentry variables. Authentication uses the
[GCP workflow credentials](continuous-integration.md#gcp-stateful-tests).

## Image publication

The [image builder](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zfnd-build-docker-image.yml#L176-L200)
publishes to `us-docker.pkg.dev/<GCP_PROJECT>/zebra/<image_name>`.
The `GCP_PROJECT` variable comes from the selected GitHub environment.

| Caller | Image name | Dockerfile target |
| --- | --- | --- |
| [`release-binaries.yml`](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/release-binaries.yml#L36-L50) | `zebra` | `runtime` |
| [`zfnd-deploy-nodes-gcp.yml`](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zfnd-deploy-nodes-gcp.yml#L238-L246) | `zebrad` | `runtime` |
| [`zfnd-ci-integration-tests-gcp.yml`](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zfnd-ci-integration-tests-gcp.yml#L156-L166) | Value of `CI_IMAGE_NAME` | `tests` |

Docker Hub publication additionally requires `publish_to_dockerhub=true`,
a `release` event, and `prerelease=false`. Only the release-binaries caller
enables it. Nonrelease builds publish only to GAR, not Docker Hub or GitHub
Container Registry (GHCR); the builder has no GHCR destination.

The tag recipes produce these development tags; matching conditions combine:

| Ref or event | Tag |
| --- | --- |
| Pull-request ref | `pr-<number>` |
| Branch ref, including manual dispatch from a branch | Sanitized branch name |
| Default-branch ref | `edge` |
| Scheduled run | `nightly`, the default `type=schedule` pattern |
| Any event with a commit SHA | `sha-<first-seven-characters>` |

The pinned [metadata action](https://github.com/docker/metadata-action/blob/030e881283bb7a6894de51c315a6bfe6a94e05cf/README.md#tags-input)
defines these defaults. Its SHA recipe ignores the `event` attribute, so
release builds also receive a SHA tag. For version tags and `latest`, see
[Docker images](../user/docker.md#images-and-tags).

## Zonal instance groups

A managed instance group (MIG) maintains the requested VM configuration.
Zebra uses one zonal MIG per environment, branch identity, network, and
zone. Each new MIG has one instance and one persistent chain-state disk.
The supported zones are `us-east1-b`, `us-east1-c`, and `us-east1-d`.
Resource names use lowercase `mainnet` or `testnet` and the final zone
letter.

| Environment | Project | MIG | State disk |
| --- | --- | --- | --- |
| `prod` | `zfnd-prod-zebra` | `zebrad-${network}-${zone-letter}` | `zebrad-cache-${network}-${zone-letter}` |
| `stage` | `zfnd-dev-zebra` | `zebrad-main-${network}-${zone-letter}` | `zebrad-cache-main-${network}-${zone-letter}` |
| `next` | `zfnd-dev-zebra` | `zebrad-next-${network}-${zone-letter}` | `zebrad-cache-next-${network}-${zone-letter}` |
| `dev` | `zfnd-dev-zebra` | `zebrad-${ref-slug}-${network}-${zone-letter}` | `zebrad-cache-${ref-slug}-${network}-${zone-letter}` |

Manual dispatch selects the source ref independently of the destination.
Stable destinations replace their MIGs even from a feature branch; check
the [dispatch targets](gcp-deployment-operations.md#run-one).

## Update mechanics

The reusable workflow validates its image, source SHA, environment, network
list, and zone list. It then ensures the network health checks exist and
resolves cached state separately for each network when requested.

Each `deploy-nodes` matrix job:

1. Rejects a disk held by an instance belonging to another MIG.
2. Reuses the named state disk, or creates a 400 GB `pd-balanced` disk from
   a matching cache image. A missing disk without a cache image is an
   error; the workflow does not create an empty disk.
3. Creates or reuses an instance template whose name includes a digest of
   its configuration. The template runs the immutable container image on
   a Spot VM using `cos-cloud/cos-stable` and mounts the state disk at
   `/home/zebra/.cache/zebra`.
4. Creates a size-one MIG if absent. The disk's stateful policy preserves
   it across VM replacement but deletes it on permanent instance deletion.
5. Updates an existing MIG with `--replacement-method=recreate`,
   `--max-surge=0`, and `--max-unavailable=1`.
6. Waits for the requested template, then optionally for group stability;
   see [deployment verification](gcp-deployment-operations.md#deploy-success-has-two-stages).

A per-MIG concurrency group covers both waits and does not cancel an active
deployment. Matrix `fail-fast` is disabled. The six network/zone jobs run
independently: **the workflow does not serialize zones**. The one-instance
unavailability limit applies within each zonal MIG, not across a network's
three MIGs. Use single-zone dispatches when the operation requires other
zones to remain untouched.

Manual dispatches omit the stability wait. Check application health before
serving clients.

## State and addresses

Push deployments request cached images for new disks. Release deployments
disable cache lookup and require pre-existing disks. Manual dispatch
defaults to `need_cached_disk=true` and `cached_disk_type=tip`;
`need_cached_disk=false` means reuse a disk already present, not initialize
an empty one. Existing disks do not require a matching image. The runbook
covers [cache-image lookup](gcp-deployment-operations.md#cache-images) and
[manual bootstrapping](gcp-deployment-operations.md#bootstrap-a-disk-without-a-cache-image).

Stable deployments use [reserved external IPs](gcp-deployment-operations.md#static-ips).
Other feature-branch `dev` deployments use `--no-address` and do not set
`ZEBRA_NETWORK__EXTERNAL_ADDR`; outbound connectivity uses the configured
subnet.

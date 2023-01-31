# Zebra with Docker

The easiest way to run Zebra is using [Docker](https://docs.docker.com/get-docker/).

We've embraced Docker in Zebra for most of the solution lifecycle, from development environments to CI (in our pipelines), and deployment to end users.

## Quick usage

You can deploy Zebra for a daily use with the images available in [Docker Hub](https://hub.docker.com/repository/docker/zfnd/zebra) or build it locally for testing

### Ready to use image

```shell
docker run --detach zfnd/zebra:1.0.0-rc.4
```

### Build it locally

```shell
git clone --depth 1 --branch v1.0.0-rc.4 https://github.com/ZcashFoundation/zebra.git
docker build --file docker/Dockerfile --target runtime --tag zebra:local
docker run --detach zebra:local
```

### Alternatives

See the Zebra [build instructions](https://github.com/ZcashFoundation/zebra#build-instructions).

## Images

The Zebra team builds multiple images with a single [Dockerfile](https://github.com/ZcashFoundation/zebra/blob/main/docker/Dockerfile) using [multistage builds](https://docs.docker.com/build/building/multi-stage/). The `test` stage adds needed features and tools (like [lightwalletd](https://github.com/adityapk00/lightwalletd)) and the `runtime` stage just adds the *zebrad* binary and required *zcash-params* for Zebra to run correctly.

As a result the Zebra team builds four images:

- [zcash-params](us-docker.pkg.dev/zealous-zebra/zebra/zcash-params): built Zcash network parameters
- [lightwalletd](us-docker.pkg.dev/zealous-zebra/zebra/lightwalletd): a backend service that provides an interface to the Zcash blockchain
- [zebrad-test](us-docker.pkg.dev/zealous-zebra/zebra/zebrad-test): a zebrad binary with lightwalletd included, and test suites enabled
- [zebra](https://hub.docker.com/repository/docker/zfnd/zebra): a streamlined version with the zebrad binary and just the needed features needed to run *as-is*

## Registries

The images built by the Zebra team are all publicly hosted. Old image versions meant to be used by our [CI pipeline](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/continous-integration-docker.yml) (`zebrad-test`, `lighwalletd`) might be deleted on a scheduled basis.

We use [Docker Hub](https://hub.docker.com/repository/docker/zfnd/zebra) for end-user images and [Google Artifact Registry](https://console.cloud.google.com/artifacts/docker/zealous-zebra/us/zebra) to build external tools and test images

# Zebra with Docker

We've embraced Docker in Zebra for most of the solution lifecycle, from development environments to CI (in our pipelines), and deployment to end users. Zebra can be built from source using Cargo, Rust’s build system and package manager alternatively, you can easily deploy Zebra using [Docker](https://docs.docker.com/get-docker/)

## Quick usage

You can deploy Zebra for a daily use with the images available in [Docker Hub](https://hub.docker.com/repository/docker/zfnd/zebra) or build it locally for testing

### Ready to use image

```shell
docker run -d zfnd/zebra:1.0.0-rc.0
```

### Build it locally

```shell
git clone --depth 1 --branch v1.0.0-rc.0 git@github.com:ZcashFoundation/zebra.git
docker build -f docker/Dockerfile --target runtime -t zebra:local
docker run -d zebra:local
```

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

# Zebra with Docker

The easiest way to run Zebra is using [Docker](https://docs.docker.com/get-docker/).

We've embraced Docker in Zebra for most of the solution lifecycle, from development environments to CI (in our pipelines), and deployment to end users.

## Quick usage

You can deploy Zebra for a daily use with the images available in [Docker Hub](https://hub.docker.com/r/zfnd/zebra) or build it locally for testing

### Ready to use image

```shell
docker run --detach zfnd/zebra:latest
```

### Build it locally

```shell
git clone --depth 1 --branch v1.1.0 https://github.com/ZcashFoundation/zebra.git
docker build --file docker/Dockerfile --target runtime --tag zebra:local .
docker run --detach zebra:local
```

### Alternatives

See [Building Zebra](https://github.com/ZcashFoundation/zebra#building-zebra) for more information.

## Registries

The images built by the Zebra team are all publicly hosted. Old image versions meant to be used by our [CI pipeline](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/continous-integration-docker.yml) (`zebrad-test`, `lighwalletd`) might be deleted on a scheduled basis.

We use [Docker Hub](https://hub.docker.com/r/zfnd/zebra) for end-user images and [Google Artifact Registry](https://console.cloud.google.com/artifacts/docker/zfnd-dev-zebra/us/zebra) to build external tools and test images

# Zebra with Docker

The easiest way to run Zebra is using [Docker](https://docs.docker.com/get-docker/).

We've embraced Docker in Zebra for most of the solution lifecycle, from development environments to CI (in our pipelines), and deployment to end users.

## Quick usage

You can deploy Zebra for daily use with the images available in [Docker Hub](https://hub.docker.com/r/zfnd/zebra) or build it locally for testing.

### Ready to use image

```shell
docker run --detach zfnd/zebra:latest
```

### Build it locally

```shell
git clone --depth 1 --branch v1.3.0 https://github.com/ZcashFoundation/zebra.git
docker build --file docker/Dockerfile --target runtime --tag zebra:local .
docker run --detach zebra:local
```

### Alternatives

See [Building Zebra](https://github.com/ZcashFoundation/zebra#building-zebra) for more information.

## Advanced usage

You're able to specify various parameters when building or launching the Docker image, which are meant to be used by developers and CI pipelines. For example, specifying the Network where Zebra will run (Mainnet, Testnet, etc), or enabling features like metrics with Prometheus.

For example, if we'd like to enable metrics on the image, we'd build it using the following `build-arg`:

> [!WARNING]
> To fully use and display the metrics, you'll need to run a Prometheus and Grafana server, and configure it to scrape and visualize the metrics endpoint. This is explained in more detailed in the [Metrics](https://zebra.zfnd.org/user/metrics.html#zebra-metrics) section of the User Guide.

```shell
docker build -f ./docker/Dockerfile --target runtime --build-arg FEATURES='default-release-binaries prometheus' --tag local/zebra.mining:latest .
```

To increase the log output we can optionally add these `build-arg`s:

```shell
--build-arg RUST_BACKTRACE=full --build-arg RUST_LOG=debug --build-arg COLORBT_SHOW_HIDDEN=1
```

And after our image has been built, we can run it on `Mainnet` with the following command, which will expose the metrics endpoint on port `9999` and force the logs to be colored:

```shell
docker run --env LOG_COLOR="true" -p 9999:9999 local/zebra.mining
```

Based on our actual `entrypoint.sh` script, the following configuration file will be generated (on the fly, at startup) and used by Zebra:

```toml
[network]
network = "Mainnet"
listen_addr = "0.0.0.0"
[state]
cache_dir = "/var/cache/zebrad-cache"
[metrics]
endpoint_addr = "127.0.0.1:9999"
```

### Build time arguments

#### Configuration

- `FEATURES`: Specifies the features to build `zebrad` with. Example: `"default-release-binaries getblocktemplate-rpcs"`

#### Logging

- `RUST_LOG`: Sets the trace log level. Example: `"debug"`
- `RUST_BACKTRACE`: Enables or disables backtraces. Example: `"full"`
- `RUST_LIB_BACKTRACE`: Enables or disables library backtraces. Example: `1`
- `COLORBT_SHOW_HIDDEN`: Enables or disables showing hidden backtraces. Example: `1`

#### Tests

- `TEST_FEATURES`: Specifies the features for tests. Example: `"lightwalletd-grpc-tests zebra-checkpoints"`
- `ZEBRA_SKIP_IPV6_TESTS`: Skips IPv6 tests. Example: `1`
- `ENTRYPOINT_FEATURES`: Overrides the specific features used to run tests in `entrypoint.sh`. Example: `"default-release-binaries lightwalletd-grpc-tests"`

#### CI/CD

- `SHORT_SHA`: Represents the short SHA of the commit. Example: `"a1b2c3d"`

### Run time variables

#### Network

- `NETWORK`: Specifies the network type. Example: `"Mainnet"`

#### Zebra Configuration

- `ZEBRA_CHECKPOINT_SYNC`: Enables or disables checkpoint sync. Example: `true`
- `ZEBRA_LISTEN_ADDR`: Address for Zebra to listen on. Example: `"0.0.0.0"`
- `ZEBRA_CACHED_STATE_DIR`: Directory for cached state. Example: `"/var/cache/zebrad-cache"`

#### Mining Configuration

- `RPC_LISTEN_ADDR`: Address for RPC to listen on. Example: `"0.0.0.0"`
- `RPC_PORT`: Port for RPC. Example: `8232`
- `MINER_ADDRESS`: Address for the miner. Example: `"t1XhG6pT9xRqRQn3BHP7heUou1RuYrbcrCc"`

#### Other Configuration

- `METRICS_ENDPOINT_ADDR`: Address for metrics endpoint. Example: `"0.0.0.0"`
- `METRICS_ENDPOINT_PORT`: Port for metrics endpoint. Example: `9999`
- `LOG_FILE`: Path to the log file. Example: `"/path/to/log/file.log"`
- `LOG_COLOR`: Enables or disables log color. Example: `false`
- `TRACING_ENDPOINT_ADDR`: Address for tracing endpoint. Example: `"0.0.0.0"`
- `TRACING_ENDPOINT_PORT`: Port for tracing endpoint. Example: `3000`

## Registries

The images built by the Zebra team are all publicly hosted. Old image versions meant to be used by our [CI pipeline](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/continous-integration-docker.yml) (`zebrad-test`, `lighwalletd`) might be deleted on a scheduled basis.

We use [Docker Hub](https://hub.docker.com/r/zfnd/zebra) for end-user images and [Google Artifact Registry](https://console.cloud.google.com/artifacts/docker/zfnd-dev-zebra/us/zebra) to build external tools and test images.

# Zebra with Docker

The easiest way to run Zebra is using [Docker](https://docs.docker.com/get-docker/).

We've embraced Docker in Zebra for most of the solution lifecycle, from development environments to CI (in our pipelines), and deployment to end users.

> [!TIP]
> We recommend using `docker compose` sub-command over the plain `docker` CLI, especially for more advanced use-cases like running CI locally, as it provides a more convenient and powerful way to manage multi-container Docker applications. See [CI/CD Local Testing](#cicd-local-testing) for more information, and other compose files available in the [docker](https://github.com/ZcashFoundation/zebra/tree/main/docker) folder.

## Quick usage

You can deploy Zebra for daily use with the images available in [Docker Hub](https://hub.docker.com/r/zfnd/zebra) or build it locally for testing.

### Ready to use image

Using `docker compose`:

```shell
docker compose -f docker/docker-compose.yml up
```

With plain `docker` CLI:

```shell
docker volume create zebrad-cache

docker run -d --platform linux/amd64 \
  --restart unless-stopped \
  --env-file .env \
  --mount type=volume,source=zebrad-cache,target=/var/cache/zebrad-cache \
  -p 8233:8233 \
  --memory 16G \
  --cpus 4 \
  zfnd/zebra
```

### Build it locally

```shell
git clone --depth 1 --branch v2.0.1 https://github.com/ZcashFoundation/zebra.git
docker build --file docker/Dockerfile --target runtime --tag zebra:local .
docker run --detach zebra:local
```

### Alternatives

See [Building Zebra](https://github.com/ZcashFoundation/zebra#building-zebra) for more information.

## Advanced usage

You're able to specify various parameters when building or launching the Docker image, which are meant to be used by developers and CI pipelines. For example, specifying the Network where Zebra will run (Mainnet, Testnet, etc), or enabling features like metrics with Prometheus.

For example, if we'd like to enable metrics on the image, we'd build it using the following `build-arg`:

> [!IMPORTANT]
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

### Running Zebra with Lightwalletd

To run Zebra with Lightwalletd, we recommend using the provided `docker compose` files for Zebra and Lightwalletd, which will start both services and connect them together, while exposing ports, mounting volumes, and setting environment variables.

```shell
docker compose -f docker/docker-compose.yml -f docker/docker-compose.lwd.yml up
```

### CI/CD Local Testing

To run CI tests locally, which mimics the testing done in our CI pipelines on GitHub Actions, use the `docker-compose.test.yml` file. This setup allows for a consistent testing environment both locally and in CI.

#### Running Tests Locally

1. **Setting Environment Variables**:
   - Modify the `test.env` file to set the desired test configurations.
   - For running all tests, set `RUN_ALL_TESTS=1` in `test.env`.

2. **Starting the Test Environment**:
   - Use Docker Compose to start the testing environment:

     ```shell
     docker-compose -f docker/docker-compose.test.yml up
     ```

   - This will start the Docker container and run the tests based on `test.env` settings.

3. **Viewing Test Output**:
   - The test results and logs will be displayed in the terminal.

4. **Stopping the Environment**:
   - Once testing is complete, stop the environment using:

     ```shell
     docker-compose -f docker/docker-compose.test.yml down
     ```

This approach ensures you can run the same tests locally that are run in CI, providing a robust way to validate changes before pushing to the repository.

### Build and Run Time Configuration

#### Build Time Arguments

#### Configuration

- `FEATURES`: Specifies the features to build `zebrad` with. Example: `"default-release-binaries getblocktemplate-rpcs"`
- `TEST_FEATURES`: Specifies the features for tests. Example: `"lightwalletd-grpc-tests zebra-checkpoints"`

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

#### Run Time Variables

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

Specific tests are defined in `docker/test.env` file and can be enabled by setting the corresponding environment variable to `1`.

## Registries

The images built by the Zebra team are all publicly hosted. Old image versions meant to be used by our [CI pipeline](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/ci-integration-tests-gcp.yml) (`zebrad-test`, `lighwalletd`) might be deleted on a scheduled basis.

We use [Docker Hub](https://hub.docker.com/r/zfnd/zebra) for end-user images and [Google Artifact Registry](https://console.cloud.google.com/artifacts/docker/zfnd-dev-zebra/us/zebra) to build external tools and test images.

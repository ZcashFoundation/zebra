# Zebra with Docker

The foundation maintains a Docker infrastructure for deploying and testing Zebra.

## Quick Start

To get Zebra quickly up and running, you can use an off-the-rack image from
[Docker Hub](https://hub.docker.com/r/zfnd/zebra/tags):

```shell
docker run -d \
  --name zebra \
  -p 8233:8233 \
  -v zebrad-cache:/home/zebra/.cache/zebra \
  zfnd/zebra
```

The `-p 8233:8233` flag publishes Zebra's P2P port so other Zcash nodes can
connect to yours (use `-p 18233:18233` for Testnet), and `-v` mounts a named
volume so the chain state survives container restarts.

You can also use `docker compose`, which we recommend. First get the repo:

```shell
git clone --depth 1 https://github.com/ZcashFoundation/zebra.git
cd zebra
```

Then run:

```shell
docker compose -f docker/docker-compose.yml up
```

The default compose file already exposes the Mainnet P2P port.

## Custom Images

If you want to use your own images with, for example, some opt-in compilation
features enabled, add the desired features to the `FEATURES` variable in the
`docker/.env` file and build the image:

```shell
docker build \
  --file docker/Dockerfile \
  --env-file docker/.env \
  --target runtime \
  --tag zebra:local \
  .
```

### Alternatives

See [Building Zebra](https://github.com/ZcashFoundation/zebra#manual-build) for more information.

### Building with Custom Features

Zebra supports various features that can be enabled during build time using the `FEATURES` build argument:

For example, if we'd like to enable metrics on the image, we'd build it using the following `build-arg`:

> [!IMPORTANT]
> To fully use and display the metrics, you'll need to run a Prometheus and Grafana server, and configure it to scrape and visualize the metrics endpoint. This is explained in more detailed in the [Metrics](https://zebra.zfnd.org/user/metrics.html#zebra-metrics) section of the User Guide.

```shell
# Build with specific features
docker build -f ./docker/Dockerfile --target runtime \
    --build-arg FEATURES="default-release-binaries prometheus" \
    --tag zebra:metrics .
```

All available Cargo features are listed at
<https://docs.rs/zebrad/latest/zebrad/index.html#zebra-feature-flags>.

## Configuring Zebra

Zebra uses [config-rs](https://crates.io/crates/config) to layer configuration from defaults, an optional TOML file, and `ZEBRA_`-prefixed environment variables. When running with Docker, configure Zebra using any of the following (later items override earlier ones):

1. **Provide a specific config file path:** Set the `CONFIG_FILE_PATH` environment variable to point to your config file within the container. The entrypoint will pass it to `zebrad` via `--config`.
2. **Use the default config file:** Mount a config file to `/home/zebra/.config/zebrad.toml` (for example using the `configs:` mapping in `docker-compose.yml`). This file is loaded if `CONFIG_FILE_PATH` is not set.
3. **Use environment variables:** Set `ZEBRA_`-prefixed environment variables to override settings from the config file. Examples: `ZEBRA_NETWORK__NETWORK`, `ZEBRA_RPC__LISTEN_ADDR`, `ZEBRA_RPC__ENABLE_COOKIE_AUTH`, `ZEBRA_RPC__COOKIE_DIR`, `ZEBRA_METRICS__ENDPOINT_ADDR`, `ZEBRA_MINING__MINER_ADDRESS`.

You can verify your configuration by inspecting Zebra's logs at startup.

### RPC

Zebra's RPC server is disabled by default. Enable and configure it via the TOML configuration file, or configuration environment variables:

- **Using a config file:** Add or uncomment the `[rpc]` section in your `zebrad.toml`. Set `listen_addr` (e.g., `"0.0.0.0:8232"` for Mainnet).
- **Using environment variables:** Set `ZEBRA_RPC__LISTEN_ADDR` (e.g., `0.0.0.0:8232`). To disable cookie auth, set `ZEBRA_RPC__ENABLE_COOKIE_AUTH=false`. To change the cookie directory, set `ZEBRA_RPC__COOKIE_DIR=/path/inside/container`.

**Cookie Authentication:**

By default, Zebra uses cookie-based authentication for RPC requests (`enable_cookie_auth = true`). When enabled, Zebra generates a unique, random cookie file required for client authentication.

- **Cookie Location:** By default, the cookie is stored at `<cache_dir>/.cookie`, where `<cache_dir>` is Zebra's cache directory (for the `zebra` user in the container this is typically `/home/zebra/.cache/zebra/.cookie`).
- **Viewing the Cookie:** If the container is running and RPC is enabled with authentication, you can view the cookie content using:

  ```bash
  docker exec <container_name> cat /home/zebra/.cache/zebra/.cookie
  ```

  (Replace `<container_name>` with your container's name, typically `zebra` if using the default `docker-compose.yml`). Your RPC client will need this value.

- **Disabling Authentication:** If you need to disable cookie authentication (e.g., for compatibility with tools like `lightwalletd`):
  - If using a **config file**, set `enable_cookie_auth = false` within the `[rpc]` section:

    ```toml
    [rpc]
    # listen_addr = ...
    enable_cookie_auth = false
    ```

  - If using **environment variables**, set `ZEBRA_RPC__ENABLE_COOKIE_AUTH=false`.

Remember that Zebra only generates the cookie file if the RPC server is enabled _and_ `enable_cookie_auth` is set to `true` (or omitted, as `true` is the default).

Environment variable examples for health endpoints:

- `ZEBRA_HEALTH__LISTEN_ADDR=0.0.0.0:8080`
- `ZEBRA_HEALTH__MIN_CONNECTED_PEERS=1`
- `ZEBRA_HEALTH__READY_MAX_BLOCKS_BEHIND=2`
- `ZEBRA_HEALTH__ENFORCE_ON_TEST_NETWORKS=false`

### Health Endpoints

Zebra can expose two lightweight HTTP endpoints for liveness and readiness:

- `GET /healthy`: returns `200 OK` when the process is up and has at least the configured number of recently live peers; otherwise `503`.
- `GET /ready`: returns `200 OK` when the node is near the tip and within the configured lag threshold; otherwise `503`.

Enable the endpoints by adding a `[health]` section to your config (see the default Docker config at `docker/default-zebra-config.toml`):

```toml
[health]
listen_addr = "0.0.0.0:8080"
min_connected_peers = 1
ready_max_blocks_behind = 2
enforce_on_test_networks = false
```

If you want to expose the endpoints to the host, add a port mapping to your compose file:

```yaml
ports:
  - "8080:8080" # Health endpoints (/healthy, /ready)
```

For Kubernetes, configure liveness and readiness probes against `/healthy` and `/ready` respectively. See the [Health Endpoints](./health.md) page for details.

### P2P Networking

Zebra uses TCP port 8233 on Mainnet and 18233 on Testnet for peer-to-peer connections. When running in Docker, publish this port with `-p` (as shown in the [Quick Start](#quick-start)) so other nodes can connect to yours. Without it, Zebra still syncs via outbound connections but does not accept inbound peers.

If Zebra is behind a NAT, firewall, or load balancer, set `external_addr` so it advertises your public address to peers instead of the internal bind address:

```toml
[network]
external_addr = "203.0.113.42:8233"
```

Or via environment variable:

```shell
-e ZEBRA_NETWORK__EXTERNAL_ADDR=203.0.113.42:8233
```

For reference, the ports Zebra can use are:

| Port  | Protocol | Purpose            | Default  |
|-------|----------|--------------------|----------|
| 8233  | TCP      | P2P (Mainnet)      | Enabled  |
| 18233 | TCP      | P2P (Testnet)      | Enabled  |
| 8232  | TCP      | RPC (Mainnet)      | Disabled |
| 18232 | TCP      | RPC (Testnet)      | Disabled |
| 9999  | TCP      | Prometheus metrics | Disabled |
| 8080  | TCP      | Health endpoints   | Disabled |

## Examples

To make the initial setup of Zebra with other services easier, we provide some
example files for `docker compose`. The following subsections will walk you
through those examples.

### Running Zebra with Lightwalletd

The following command will run Zebra with Lightwalletd:

```shell
docker compose -f docker/docker-compose.lwd.yml up
```

Note that Docker will run Zebra with the RPC server enabled and the cookie
authentication mechanism disabled when running `docker compose -f docker/docker-compose.lwd.yml up`, since Lightwalletd doesn't support cookie authentication. In this
example, the RPC server is configured by setting `ZEBRA_` environment variables
directly in `docker/docker-compose.lwd.yml` (or an accompanying `.env` file).

### Running Zebra with Prometheus and Grafana

The following commands will run Zebra with the observability stack (Prometheus,
Grafana, Jaeger, and AlertManager):

```shell
docker compose -f docker/docker-compose.observability.yml build --no-cache
docker compose -f docker/docker-compose.observability.yml up
```

This builds a local Zebra image with the `opentelemetry` Cargo feature and
starts all observability services. Once running:

- Grafana: `http://localhost:3000` (default login: admin/admin)
- Prometheus: `http://localhost:9094`
- Jaeger: `http://localhost:16686`
- Zebra metrics: `http://localhost:9999`

See `docker/observability/README.md` for dashboard setup and configuration.

### Running CI Tests Locally

To run CI tests locally, first set the variables in the `test.env` file to
configure the tests, then run:

```shell
docker-compose -f docker/docker-compose.test.yml up
```

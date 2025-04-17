# Zebra with Docker

The foundation maintains a Docker infrastructure for deploying and testing Zebra.

## Quick Start

To get Zebra quickly up and running, you can use an off-the-rack image from
[Docker Hub](https://hub.docker.com/r/zfnd/zebra/tags):

```shell
docker run --name zebra zfnd/zebra
```

If you want to preserve Zebra's state, you can create a Docker volume:

```shell
docker volume create zebrad-cache
```

And mount it before you start the container:

```shell
docker run \
  --mount source=zebrad-cache,target=/home/zebra/.cache/zebra \
  --name zebra \
  zfnd/zebra
```

You can also use `docker compose`, which we recommend. First get the repo:

```shell
git clone --depth 1 --branch v2.2.0 https://github.com/ZcashFoundation/zebra.git
cd zebra
```

Then run:

```shell
docker compose -f docker/docker-compose.yml up
```

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

To configure Zebra using Docker, you have a few options, processed in this order:

1. **Provide a specific config file path:** Set the `ZEBRA_CONF_PATH` environment variable to point to your config file within the container.
2. **Use the default config file:** By default, the `docker-compose.yml` file mounts `./default-zebra-config.toml` to `/home/zebra/.config/zebrad.toml` using the `configs:` mapping. Zebra will use this file if `ZEBRA_CONF_PATH` is not set. To use environment variables instead, you must **comment out** the `configs:` mapping in `docker/docker-compose.yml`.
3. **Generate config from environment variables:** If neither of the above methods provides a config file (i.e., `ZEBRA_CONF_PATH` is unset *and* the `configs:` mapping in `docker-compose.yml` is commented out), the container's entrypoint script will *automatically generate* a default configuration file at `/home/zebra/.config/zebrad.toml`. This generated file uses specific environment variables (like `NETWORK`, `ZEBRA_RPC_PORT`, `ENABLE_COOKIE_AUTH`, `MINER_ADDRESS`, etc.) to define the settings. Using the `docker/.env` file is the primary way to set these variables for this auto-generation mode.

You can see if your config works as intended by looking at Zebra's logs.

Note that if you provide a configuration file using methods 1 or 2, environment variables from `docker/.env` will **not** override the settings within that file. The environment variables are primarily used for the auto-generation scenario (method 3).

### RPC

Zebra's RPC server is disabled by default. To enable it, you need to define the RPC settings in Zebra's configuration. You can achieve this using one of the configuration methods described above:

* **Using a config file (methods 1 or 2):** Add or uncomment the `[rpc]` section in your `zebrad.toml` file (like the one provided in `docker/default-zebra-config.toml`). Ensure you set the `listen_addr` (e.g., `"0.0.0.0:8232"` for Mainnet).
* **Using environment variables (method 3):** Set the `ZEBRA_RPC_PORT` environment variable (e.g., in `docker/.env`). This tells the entrypoint script to include an enabled `[rpc]` section listening on `0.0.0.0:<ZEBRA_RPC_PORT>` in the auto-generated configuration file.

**Cookie Authentication:**

By default, Zebra uses cookie-based authentication for RPC requests (`enable_cookie_auth = true`). When enabled, Zebra generates a unique, random cookie file required for client authentication.

* **Cookie Location:** The entrypoint script configures Zebra to store this file at `/home/zebra/.cache/zebra/.cookie` inside the container.
* **Viewing the Cookie:** If the container is running and RPC is enabled with authentication, you can view the cookie content using:

    ```bash
    docker exec <container_name> cat /home/zebra/.cache/zebra/.cookie
    ```

    (Replace `<container_name>` with your container's name, typically `zebra` if using the default `docker-compose.yml`). Your RPC client will need this value.
* **Disabling Authentication:** If you need to disable cookie authentication (e.g., for compatibility with tools like `lightwalletd`):
  * If using a **config file** (methods 1 or 2), set `enable_cookie_auth = false` within the `[rpc]` section:

    ```toml
    [rpc]
    # listen_addr = ...
    enable_cookie_auth = false
    ```

  * If using **environment variables** for auto-generation (method 3), set `ENABLE_COOKIE_AUTH=false` in your `docker/.env` file.

Remember that Zebra only generates the cookie file if the RPC server is enabled *and* `enable_cookie_auth` is set to `true` (or omitted, as `true` is the default).

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
authentication mechanism disabled since Lightwalletd doesn't support it. Instead
of configuring Zebra via the recommended config file or `docker/.env` file, we
configured the RPC server by setting environment variables directly in the
`docker/docker-compose.lwd.yml` file. This takes advantage of the entrypoint
script's auto-generation feature (method 3 described above).

### Running Zebra with Prometheus and Grafana

The following commands will run Zebra with Prometheus and Grafana:

```shell
docker compose -f docker/docker-compose.grafana.yml build --no-cache
docker compose -f docker/docker-compose.grafana.yml up
```

In this example, we build a local Zebra image with the `prometheus` Cargo
compilation feature. Note that we enable this feature by specifying its name in
the build arguments. Having this Cargo feature specified at build time makes
`cargo` compile Zebra with the metrics support for Prometheus enabled. Note that
we also specify this feature as an environment variable at run time. Having this
feature specified at run time makes Docker's entrypoint script configure Zebra
to open a scraping endpoint on `localhost:9999` for Prometheus.

Once all services are up, the Grafana web UI should be available at
`localhost:3000`, the Prometheus web UI should be at `localhost:9090`, and
Zebra's scraping page should be at `localhost:9999`. The default login and
password for Grafana are both `admin`. To make Grafana use Prometheus, you need
to add Prometheus as a data source with the URL `http://localhost:9090` in
Grafana's UI. You can then import various Grafana dashboards from the `grafana`
directory in the Zebra repo.

### Running CI Tests Locally

To run CI tests locally, first set the variables in the `test.env` file to
configure the tests, then run:

```shell
docker-compose -f docker/docker-compose.test.yml up
```

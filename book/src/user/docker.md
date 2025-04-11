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

To configure Zebra, edit the `docker/default-zebra-config.toml` config file and
uncomment the `configs` mapping in `docker/docker-compose.yml` so your config
takes effect. You can see if your config works as intended by looking at Zebra's
logs.

Alternatively, you can configure Zebra by setting the environment variables in
the `docker/.env` file. Note that the config options of this file are limited to
the variables already present in the commented out blocks in it and adding new
ones will not be effective. Also note that the values of the variables take
precedence over the values set in the `docker/default-zebra-config.toml` config
file. The `docker/.env` file serves as a quick way to override the most commonly
used settings for Zebra, whereas the `docker/default-zebra-config.toml` file
provides full config capabilities.

### RPC

Zebra's RPC server is disabled by default. To enable it, you need to set its RPC
port. You can do that either in the `docker/default-zebra-config.toml` file or
`docker/.env` file, as described in the two paragraphs above.

When connecting to Zebra's RPC server, your RPC clients need to provide an
authentication cookie to the server or you need to turn the authentication off
in Zebra's config. By default, the cookie file is stored at
`/home/zebra/.cache/zebra/.cookie` in the container. You can print its contents
by running

```bash
docker exec zebra cat /home/zebra/.cache/zebra/.cookie
```

when the `zebra` container is running. Note that Zebra generates the cookie file
only if the RPC server is enabled, and each Zebra instance generates a unique
one. To turn the authentication off, either set `ENABLE_COOKIE_AUTH=false` in
`docker/.env` or set

```toml
[rpc]
enable_cookie_auth = false
```

in `docker/default-zebra-config.toml` and mount this config file into the
container's filesystem in `docker/docker-compose.yml` as described at the
beginning of this section.

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
`docker/docker-compose.lwd.yml` file. This is a shortcut we can take when we are
familiar with the `docker/.env` file.

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

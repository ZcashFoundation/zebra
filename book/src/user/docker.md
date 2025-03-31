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

All available Cargo features are listed at
<https://docs.rs/zebrad/latest/zebrad/index.html#zebra-feature-flags>.

## Configuring Zebra

To configure Zebra, you have three options:

1. Edit the `docker/default-zebra-config.toml` config file and uncomment the `configs` mapping in `docker/docker-compose.yml` so your config takes effect.
2. Provide a path to your own config file via the `ZEBRA_CONF_PATH` environment variable.
3. Let Zebra generate a default config file automatically from environment variables (if neither of the above methods is used).

The entrypoint script follows this sequence:

1. If `ZEBRA_CONF_PATH` is set and a file exists at that path, it uses that file
2. If `ZEBRA_CONF_PATH` is not set but a default config exists at `${HOME}/.config/zebrad.toml`, it uses that file
3. If neither exists, it generates a default config at `${HOME}/.config/zebrad.toml` based on environment variables
4. Environment variables from the `docker/.env` only take effect if a configuration file is not mounted, and thus are only effective if you don't provide a custom config file.

You can see if your config works as intended by looking at Zebra's logs.

Note that the environment variable overrides in the `docker/.env` file are limited to the variables already present in the commented out blocks and adding new ones will not be effective. The `docker/.env` file serves as a quick way to override the most commonly used settings, whereas a custom config file provides full configuration capabilities.

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

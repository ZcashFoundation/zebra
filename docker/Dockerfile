# syntax=docker/dockerfile:1
# check=skip=UndefinedVar,UserExist # We use gosu in the entrypoint instead of USER directive

# If you want to include a file in the Docker image, add it to .dockerignore.
#
# We are using 4 (TODO: 5) stages:
# - deps: installs build dependencies and sets default values
# - tests: builds tests binaries
# - release: builds release binaries
# - runtime: runs the release binaries
# - TODO: Add a `monitoring` stage
#
# We first set default values for build arguments used across the stages.
# Each stage must define the build arguments (ARGs) it uses.

# Build zebrad with these features
#
# Keep these argument defaults in sync with GitHub vars.RUST_PROD_FEATURES
# https://github.com/ZcashFoundation/zebra/settings/variables/actions
ARG FEATURES="default-release-binaries"

ARG USER="zebra"
ARG UID=10001
ARG GID=10001
ARG HOME="/home/${USER}"

ARG RUST_VERSION=1.85.0
# In this stage we download all system requirements to build the project
#
# It also captures all the build arguments to be used as environment variables.
# We set defaults for the arguments, in case the build does not include this information.
FROM rust:${RUST_VERSION}-bookworm AS deps
SHELL ["/bin/bash", "-xo", "pipefail", "-c"]

# Install zebra build deps
RUN apt-get -qq update && \
    apt-get -qq install -y --no-install-recommends \
    libclang-dev \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/* /tmp/*

# Build arguments and variables set for tracelog levels and debug information
ARG RUST_LOG
ENV RUST_LOG=${RUST_LOG:-info}

ARG RUST_BACKTRACE
ENV RUST_BACKTRACE=${RUST_BACKTRACE:-1}

ARG RUST_LIB_BACKTRACE
ENV RUST_LIB_BACKTRACE=${RUST_LIB_BACKTRACE:-1}

ARG COLORBT_SHOW_HIDDEN
ENV COLORBT_SHOW_HIDDEN=${COLORBT_SHOW_HIDDEN:-1}

ARG CARGO_INCREMENTAL
ENV CARGO_INCREMENTAL=${CARGO_INCREMENTAL:-0}

ARG SHORT_SHA
# If this is not set, it must be an empty string, so Zebra can try an alternative git commit source:
# https://github.com/ZcashFoundation/zebra/blob/9ebd56092bcdfc1a09062e15a0574c94af37f389/zebrad/src/application.rs#L179-L182
ENV SHORT_SHA=${SHORT_SHA:-}

# This stage builds tests without running them.
#
# We also download needed dependencies for tests to work, from other images.
# An entrypoint.sh is only available in this step for easier test handling with variables.
FROM deps AS tests

ARG FEATURES
ENV FEATURES=${FEATURES}

# Skip IPv6 tests by default, as some CI environment don't have IPv6 available
ARG ZEBRA_SKIP_IPV6_TESTS
ENV ZEBRA_SKIP_IPV6_TESTS=${ZEBRA_SKIP_IPV6_TESTS:-1}

# This environment setup is almost identical to the `runtime` target so that the
# `tests` target differs minimally. In fact, a subset of this setup is used for
# the `runtime` target.
ARG UID
ENV UID=${UID}
ARG GID
ENV GID=${GID}
ARG HOME
ENV HOME=${HOME}
ARG USER
ENV USER=${USER}

RUN addgroup --gid ${GID} ${USER} && \
    adduser --gid ${GID} --uid ${UID} --home ${HOME} ${USER}

# Set the working directory for the build.
WORKDIR ${HOME}
ENV CARGO_HOME="${HOME}/.cargo/"

# Build Zebra test binaries, but don't run them

# Leverage a cache mount to /usr/local/cargo/registry/
# for downloaded dependencies, a cache mount to /usr/local/cargo/git/db
# for git repository dependencies, and a cache mount to ${HOME}/target/ for
# compiled dependencies which will speed up subsequent builds.
# Leverage a bind mount to each crate directory to avoid having to copy the
# source code into the container. Once built, copy the executable to an
# output directory before the cache mounted ${HOME}/target/ is unmounted.
RUN --mount=type=bind,source=zebrad,target=zebrad \
    --mount=type=bind,source=zebra-chain,target=zebra-chain \
    --mount=type=bind,source=zebra-network,target=zebra-network \
    --mount=type=bind,source=zebra-state,target=zebra-state \
    --mount=type=bind,source=zebra-script,target=zebra-script \
    --mount=type=bind,source=zebra-consensus,target=zebra-consensus \
    --mount=type=bind,source=zebra-rpc,target=zebra-rpc \
    --mount=type=bind,source=zebra-node-services,target=zebra-node-services \
    --mount=type=bind,source=zebra-test,target=zebra-test \
    --mount=type=bind,source=zebra-utils,target=zebra-utils \
    --mount=type=bind,source=zebra-scan,target=zebra-scan \
    --mount=type=bind,source=zebra-grpc,target=zebra-grpc \
    --mount=type=bind,source=tower-batch-control,target=tower-batch-control \
    --mount=type=bind,source=tower-fallback,target=tower-fallback \
    --mount=type=bind,source=Cargo.toml,target=Cargo.toml \
    --mount=type=bind,source=Cargo.lock,target=Cargo.lock \
    --mount=type=cache,target=${HOME}/target/ \
    --mount=type=cache,target=/usr/local/cargo/git/db \
    --mount=type=cache,target=/usr/local/cargo/registry/ \
    cargo test --locked --release --workspace --no-run \
    --features "${FEATURES} zebra-checkpoints" && \
    cp ${HOME}/target/release/zebrad /usr/local/bin && \
    cp ${HOME}/target/release/zebra-checkpoints /usr/local/bin

# Copy the lightwalletd binary and source files to be able to run tests
COPY --from=electriccoinco/lightwalletd:latest /usr/local/bin/lightwalletd /usr/local/bin/

# Copy the gosu binary to be able to run the entrypoint as non-root user
# and allow to change permissions for mounted cache directories
COPY --from=tianon/gosu:bookworm /gosu /usr/local/bin/

# Use the same default config as in the production environment.
ENV ZEBRA_CONF_PATH="${HOME}/.config/zebrad.toml"
COPY --chown=${UID}:${GID} ./docker/default-zebra-config.toml ${ZEBRA_CONF_PATH}

ENV LWD_CACHE_DIR="${HOME}/.cache/lwd"
RUN mkdir -p ${LWD_CACHE_DIR} && \
    chown -R ${UID}:${GID} ${LWD_CACHE_DIR}

# Use the same cache dir as in the production environment.
ENV ZEBRA_CACHE_DIR="${HOME}/.cache/zebra"
RUN mkdir -p ${ZEBRA_CACHE_DIR} && \
    chown -R ${UID}:${GID} ${ZEBRA_CACHE_DIR}

COPY ./ ${HOME}
RUN chown -R ${UID}:${GID} ${HOME}

COPY ./docker/entrypoint.sh /usr/local/bin/entrypoint.sh

ENTRYPOINT [ "entrypoint.sh", "test" ]

# In this stage we build a release (generate the zebrad binary)
#
# This step also adds `cache mounts` as this stage is completely independent from the
# `test` stage. This step is a dependency for the `runtime` stage, which uses the resulting
# zebrad binary from this step.
FROM deps AS release

ARG FEATURES
ARG HOME

RUN --mount=type=bind,source=tower-batch-control,target=tower-batch-control \
    --mount=type=bind,source=tower-fallback,target=tower-fallback \
    --mount=type=bind,source=zebra-chain,target=zebra-chain \
    --mount=type=bind,source=zebra-consensus,target=zebra-consensus \
    --mount=type=bind,source=zebra-grpc,target=zebra-grpc \
    --mount=type=bind,source=zebra-network,target=zebra-network \
    --mount=type=bind,source=zebra-node-services,target=zebra-node-services \
    --mount=type=bind,source=zebra-rpc,target=zebra-rpc \
    --mount=type=bind,source=zebra-scan,target=zebra-scan \
    --mount=type=bind,source=zebra-script,target=zebra-script \
    --mount=type=bind,source=zebra-state,target=zebra-state \
    --mount=type=bind,source=zebra-test,target=zebra-test \
    --mount=type=bind,source=zebra-utils,target=zebra-utils \
    --mount=type=bind,source=zebrad,target=zebrad \
    --mount=type=bind,source=Cargo.toml,target=Cargo.toml \
    --mount=type=bind,source=Cargo.lock,target=Cargo.lock \
    --mount=type=cache,target=${HOME}/target/ \
    --mount=type=cache,target=/usr/local/cargo/git/db \
    --mount=type=cache,target=/usr/local/cargo/registry/ \
    cargo build --locked --release --features "${FEATURES}" --package zebrad --bin zebrad && \
    cp ${HOME}/target/release/zebrad /usr/local/bin

# This step starts from scratch using Debian and only adds the resulting binary
# from the `release` stage.
FROM debian:bookworm-slim AS runtime

ARG FEATURES
ENV FEATURES=${FEATURES}

# Create a non-privileged user for running `zebrad`.
#
# We use a high UID/GID (10001) to avoid overlap with host system users.
# This reduces the risk of container user namespace conflicts with host accounts,
# which could potentially lead to privilege escalation if a container escape occurs.
#
# We do not use the `--system` flag for user creation since:
# 1. System user ranges (100-999) can collide with host system users
#   (see: https://github.com/nginxinc/docker-nginx/issues/490)
# 2. There's no value added and warning messages can be raised at build time
#   (see: https://github.com/dotnet/dotnet-docker/issues/4624)
#
# The high UID/GID values provide an additional security boundary in containers
# where user namespaces are shared with the host.
ARG UID
ENV UID=${UID}
ARG GID
ENV GID=${GID}
ARG HOME
ENV HOME=${HOME}
ARG USER
ENV USER=${USER}

RUN addgroup --gid ${GID} ${USER} && \
    adduser --gid ${GID} --uid ${UID} --home ${HOME} ${USER}

WORKDIR ${HOME}

# We set the default locations of the conf and cache dirs according to the XDG
# spec: https://specifications.freedesktop.org/basedir-spec/latest/

ARG ZEBRA_CONF_PATH="${HOME}/.config/zebrad.toml"
ENV ZEBRA_CONF_PATH=${ZEBRA_CONF_PATH}
COPY --chown=${UID}:${GID} ./docker/default-zebra-config.toml ${ZEBRA_CONF_PATH}

ARG ZEBRA_CACHE_DIR="${HOME}/.cache/zebra"
ENV ZEBRA_CACHE_DIR=${ZEBRA_CACHE_DIR}
RUN mkdir -p ${ZEBRA_CACHE_DIR} && chown -R ${UID}:${GID} ${ZEBRA_CACHE_DIR}

RUN chown -R ${UID}:${GID} ${HOME}

# We're explicitly NOT using the USER directive here.
# Instead, we run as root initially and use gosu in the entrypoint.sh
# to step down to the non-privileged user. This allows us to change permissions
# on mounted volumes before running the application as a non-root user.
# User with UID=${UID} is created above and used via gosu in entrypoint.sh.

# Copy the gosu binary to be able to run the entrypoint as non-root user
COPY --from=tianon/gosu:bookworm /gosu /usr/local/bin/

COPY --from=release /usr/local/bin/zebrad /usr/local/bin/
COPY ./docker/entrypoint.sh /usr/local/bin/entrypoint.sh

ENTRYPOINT [ "entrypoint.sh" ]
CMD ["zebrad"]

# TODO: Add a `monitoring` stage
#
# This stage will be based on `runtime`, and initially:
#
# - run `zebrad` on Testnet
# - with mining enabled using S-nomp and `nheqminer`.
#
# We can add further functionality to this stage for further purposes.

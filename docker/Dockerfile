# syntax=docker/dockerfile:1
# check=skip=UndefinedVar

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
#
# Build zebrad with these features
#
# Keep these argument defaults in sync with GitHub vars.RUST_PROD_FEATURES and vars.RUST_TEST_FEATURES
# https://github.com/ZcashFoundation/zebra/settings/variables/actions
ARG FEATURES="default-release-binaries"
ARG TEST_FEATURES="lightwalletd-grpc-tests zebra-checkpoints"
ARG EXPERIMENTAL_FEATURES=""

ARG APP_HOME="/opt/zebrad"
ARG RUST_VERSION=1.84.0

# In this stage we download all system requirements to build the project
#
# It also captures all the build arguments to be used as environment variables.
# We set defaults for the arguments, in case the build does not include this information.
FROM rust:${RUST_VERSION}-bookworm AS deps
SHELL ["/bin/bash", "-xo", "pipefail", "-c"]

# Set the default path for the zebrad binary
ARG APP_HOME
ENV APP_HOME=${APP_HOME}
WORKDIR ${APP_HOME}

# Install zebra build deps and Dockerfile deps
RUN apt-get -qq update && \
    apt-get -qq install -y --no-install-recommends \
    llvm \
    libclang-dev \
    clang \
    ca-certificates \
    protobuf-compiler \
    rocksdb-tools \
    && rm -rf /var/lib/apt/lists/* /tmp/*

# Build arguments and variables set for tracelog levels and debug information
#
# We set defaults to all variables.
ARG RUST_LOG
ENV RUST_LOG=${RUST_LOG:-info}

ARG RUST_BACKTRACE
ENV RUST_BACKTRACE=${RUST_BACKTRACE:-1}

ARG RUST_LIB_BACKTRACE
ENV RUST_LIB_BACKTRACE=${RUST_LIB_BACKTRACE:-1}

ARG COLORBT_SHOW_HIDDEN
ENV COLORBT_SHOW_HIDDEN=${COLORBT_SHOW_HIDDEN:-1}

ARG SHORT_SHA
# If this is not set, it must be an empty string, so Zebra can try an alternative git commit source:
# https://github.com/ZcashFoundation/zebra/blob/9ebd56092bcdfc1a09062e15a0574c94af37f389/zebrad/src/application.rs#L179-L182
ENV SHORT_SHA=${SHORT_SHA:-}

ENV CARGO_HOME="${APP_HOME}/.cargo/"

COPY ./docker/entrypoint.sh /usr/local/bin/entrypoint.sh

# This stage builds tests without running them.
#
# We also download needed dependencies for tests to work, from other images.
# An entrypoint.sh is only available in this step for easier test handling with variables.
FROM deps AS tests

# Skip IPv6 tests by default, as some CI environment don't have IPv6 available
ARG ZEBRA_SKIP_IPV6_TESTS
ENV ZEBRA_SKIP_IPV6_TESTS=${ZEBRA_SKIP_IPV6_TESTS:-1}

# Use ENTRYPOINT_FEATURES to override the specific features used to run tests in entrypoint.sh,
# separately from the test and production image builds.
ARG FEATURES
ARG TEST_FEATURES
ARG EXPERIMENTAL_FEATURES
# TODO: add empty $EXPERIMENTAL_FEATURES when we can avoid adding an extra space to the end of the string
ARG ENTRYPOINT_FEATURES="${FEATURES} ${TEST_FEATURES}"

# Build Zebra test binaries, but don't run them

# Leverage a cache mount to /usr/local/cargo/registry/
# for downloaded dependencies, a cache mount to /usr/local/cargo/git/db
# for git repository dependencies, and a cache mount to ${APP_HOME}/target/ for
# compiled dependencies which will speed up subsequent builds.
# Leverage a bind mount to each crate directory to avoid having to copy the
# source code into the container. Once built, copy the executable to an
# output directory before the cache mounted ${APP_HOME}/target/ is unmounted.
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
    --mount=type=cache,target=${APP_HOME}/target/ \
    --mount=type=cache,target=/usr/local/cargo/git/db \
    --mount=type=cache,target=/usr/local/cargo/registry/ \
    cargo test --locked --release --features "${ENTRYPOINT_FEATURES}" --workspace --no-run && \
    cp ${APP_HOME}/target/release/zebrad /usr/local/bin && \
    cp ${APP_HOME}/target/release/zebra-checkpoints /usr/local/bin

# Copy the lightwalletd binary and source files to be able to run tests
COPY --from=electriccoinco/lightwalletd:latest /usr/local/bin/lightwalletd /usr/local/bin/
COPY ./ ./

# Entrypoint environment variables
ENV ENTRYPOINT_FEATURES=${ENTRYPOINT_FEATURES}
# We repeat the ARGs here, so they are available in the entrypoint.sh script for $RUN_ALL_EXPERIMENTAL_TESTS
ARG EXPERIMENTAL_FEATURES="journald prometheus filter-reload"
ENV ENTRYPOINT_FEATURES_EXPERIMENTAL="${ENTRYPOINT_FEATURES} ${EXPERIMENTAL_FEATURES}"

# Run tests specified by the env vars
ENTRYPOINT [ "entrypoint.sh", "test" ]

# In this stage we build a release (generate the zebrad binary)
#
# This step also adds `cache mounts` as this stage is completely independent from the
# `test` stage. This step is a dependency for the `runtime` stage, which uses the resulting
# zebrad binary from this step.
FROM deps AS release

ARG FEATURES

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
    --mount=type=cache,target=${APP_HOME}/target/ \
    --mount=type=cache,target=/usr/local/cargo/git/db \
    --mount=type=cache,target=/usr/local/cargo/registry/ \
    cargo build --locked --release --features "${FEATURES}" --package zebrad --bin zebrad && \
    cp ${APP_HOME}/target/release/zebrad /usr/local/bin

# This step starts from scratch using Debian and only adds the resulting binary
# from the `release` stage.
FROM debian:bookworm-slim AS runtime

COPY --from=release /usr/local/bin/zebrad /usr/local/bin/
COPY --from=release /usr/local/bin/entrypoint.sh /usr/local/bin/

ARG FEATURES
ENV FEATURES=${FEATURES}

# Create a non-privileged system user for running `zebrad`.
ARG USER="zebra"
ENV USER=${USER}

# System users have no home dirs, but we set one for users' convenience.
ARG HOME="/home/zebra"
WORKDIR ${HOME}

# System UIDs should be set according to
# https://refspecs.linuxfoundation.org/LSB_5.0.0/LSB-Core-generic/LSB-Core-generic/uidrange.html.
#
# In Debian, the default dynamic range for system UIDs is defined by
# [FIRST_SYSTEM_UID, LAST_SYSTEM_UID], which is set to [100, 999] in
# `etc/adduser.conf`:
# https://manpages.debian.org/bullseye/adduser/adduser.8.en.html
#
# Debian assigns GID 100 to group `users`, so we set UID = GID = 101 as the
# default value.
ARG UID=101
ENV UID=${UID}
ARG GID=${UID}
ENV GID=${GID}

RUN addgroup --system --gid ${GID} ${USER}
RUN adduser --system --gid ${GID} --uid ${UID} --home ${HOME} ${USER}

# We set the locations of the conf and cache dirs according to the XDG spec:
# https://specifications.freedesktop.org/basedir-spec/latest/

ARG ZEBRA_CONF_DIR="${HOME}/.config"
ENV ZEBRA_CONF_DIR=${ZEBRA_CONF_DIR}
RUN mkdir -p ${ZEBRA_CONF_DIR} && chown ${UID}:${GID} ${ZEBRA_CONF_DIR}

ARG ZEBRA_CACHE_DIR="${HOME}/.cache/zebra"
ENV ZEBRA_CACHE_DIR=${ZEBRA_CACHE_DIR}
RUN mkdir -p ${ZEBRA_CACHE_DIR} && chown ${UID}:${GID} ${ZEBRA_CACHE_DIR}

USER $USER

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

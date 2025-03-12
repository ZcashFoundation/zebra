# syntax=docker/dockerfile:1
# check=skip=UndefinedVar,UserExist # We use gosu in the entrypoint instead of USER directive

# If you want to include a file in the Docker image, add it to .dockerignore.
#
# We use 4 (TODO: 5) stages:
# - deps: installs build dependencies and sets default values
# - tests: prepares a test image
# - release: builds release binaries
# - runtime: prepares the release image
# - TODO: Add a `monitoring` stage
#
# We first set default values for build arguments used across the stages.
# Each stage must define the build arguments (ARGs) it uses.

ARG RUST_VERSION=1.85.0

# Keep in sync with vars.RUST_PROD_FEATURES in GitHub
# https://github.com/ZcashFoundation/zebra/settings/variables/actions
ARG FEATURES="default-release-binaries"

ARG UID=10001
ARG GID=$UID
ARG USER="zebra"
ARG HOME="/home/${USER}"
ARG CARGO_HOME="${HOME}/.cargo/"
ARG CARGO_INCREMENTAL=0

# This stage prepares Zebra's build deps and captures build args as env vars.
FROM rust:${RUST_VERSION}-bookworm AS deps
SHELL ["/bin/bash", "-xo", "pipefail", "-c"]

RUN apt-get -qq update && \
    apt-get -qq install -y --no-install-recommends \
    libclang-dev \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/* /tmp/*

ARG UID
ARG GID
ARG USER
ARG HOME

ENV UID=${UID}
ENV GID=${GID}
ENV USER=${USER}
ENV HOME=${HOME}
ENV CARGO_HOME=${CARGO_HOME}
ENV CARGO_INCREMENTAL=${CARGO_INCREMENTAL}

# Skip IPv6 tests by default, as some CI environment don't have IPv6 available
ARG ZEBRA_SKIP_IPV6_TESTS=1
ENV ZEBRA_SKIP_IPV6_TESTS=${ZEBRA_SKIP_IPV6_TESTS}

# If this is not set, it must be an empty string, so Zebra can try an
# alternative git commit source:
# https://github.com/ZcashFoundation/zebra/blob/9ebd56092bcdfc1a09062e15a0574c94af37f389/zebrad/src/application.rs#L179-L182
ARG SHORT_SHA
ENV SHORT_SHA=${SHORT_SHA:-}

# This stage builds tests without running them.
FROM deps AS tests

ARG FEATURES
ARG HOME

ARG ZEBRA_CONF_PATH="${HOME}/.config/zebrad.toml"
ENV ZEBRA_CONF_PATH=${ZEBRA_CONF_PATH}

# Build Zebra test binaries, but don't run them
#
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

COPY --from=electriccoinco/lightwalletd:latest /usr/local/bin/lightwalletd /usr/local/bin/
COPY --from=tianon/gosu:bookworm /gosu /usr/local/bin/
COPY ./docker/default-zebra-config.toml ${ZEBRA_CONF_PATH}
COPY ./docker/entrypoint.sh /usr/local/bin/entrypoint.sh
COPY ./ ${HOME}

ENTRYPOINT [ "entrypoint.sh", "test" ]

# This stage builds the zebrad release binary.
#
# It also adds `cache mounts` as this stage is completely independent from the
# `test` stage.
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

# This stage starts from scratch using Debian and copies the built zebrad binary
# from the `release` stage along with other binaries and files.
FROM debian:bookworm-slim AS runtime

ARG UID
ARG GID
ARG USER
ARG HOME

ENV UID=${UID}
ENV GID=${GID}
ENV USER=${USER}
ENV HOME=${HOME}

ARG ZEBRA_CONF_PATH="${HOME}/.config/zebrad.toml"
ENV ZEBRA_CONF_PATH=${ZEBRA_CONF_PATH}

COPY --from=release /usr/local/bin/zebrad /usr/local/bin/
COPY ./docker/entrypoint.sh /usr/local/bin/entrypoint.sh
COPY ./docker/default-zebra-config.toml ${ZEBRA_CONF_PATH}
COPY --from=tianon/gosu:bookworm /gosu /usr/local/bin/

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

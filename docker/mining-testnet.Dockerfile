# We are using five stages:
# - chef: installs cargo-chef
# - planner: computes the recipe file
# - deps: caches our dependencies and sets the needed variables
# - tests: builds tests
# - release: builds release binary
# - runtime: is our runtime environment
#
# This stage implements cargo-chef for docker layer caching
FROM rust:bullseye as chef
RUN cargo install cargo-chef --locked
WORKDIR /opt/zebrad

# Analyze the current project to determine the minimum subset of files
# (Cargo.lock and Cargo.toml manifests) required to build it and cache dependencies
#
# The recipe.json is the equivalent of the Python requirements.txt file
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# In this stage we download all system requirements to build the project
#
# It also captures all the build arguments to be used as environment variables.
# We set defaults for the arguments, in case the build does not include this information.
FROM chef AS deps
SHELL ["/bin/bash", "-xo", "pipefail", "-c"]
COPY --from=planner /opt/zebrad/recipe.json recipe.json

# Install zebra build deps
RUN apt-get -qq update && \
    apt-get -qq install -y --no-install-recommends \
    llvm \
    libclang-dev \
    clang \
    ca-certificates \
    protobuf-compiler \
    ; \
    rm -rf /var/lib/apt/lists/* /tmp/*

# Install google OS Config agent to be able to get information from the VMs being deployed
# into GCP for integration testing purposes, and as Mainnet nodes
# TODO: this shouldn't be a hardcoded requirement for everyone
RUN if [ "$(uname -m)" != "aarch64" ]; then \
      apt-get -qq update && \
      apt-get -qq install -y --no-install-recommends \
      curl \
      lsb-release \
      && \
      echo "deb http://packages.cloud.google.com/apt google-compute-engine-$(lsb_release -cs)-stable main" > /etc/apt/sources.list.d/google-compute-engine.list && \
      curl https://packages.cloud.google.com/apt/doc/apt-key.gpg | apt-key add - && \
      apt-get -qq update  && \
      apt-get -qq install -y --no-install-recommends google-osconfig-agent; \
    fi \
    && \
    rm -rf /var/lib/apt/lists/* /tmp/*

# Build arguments and variables set to change how tests are run, tracelog levels,
# and Network to be used (Mainnet or Testnet)
#
# We set defaults to all variables.
ARG RUST_BACKTRACE
ENV RUST_BACKTRACE ${RUST_BACKTRACE:-0}

ARG RUST_LIB_BACKTRACE
ENV RUST_LIB_BACKTRACE ${RUST_LIB_BACKTRACE:-0}

ARG COLORBT_SHOW_HIDDEN
ENV COLORBT_SHOW_HIDDEN ${COLORBT_SHOW_HIDDEN:-0}

ARG RUST_LOG
ENV RUST_LOG ${RUST_LOG:-info}

# Skip IPv6 tests by default, as some CI environment don't have IPv6 available
ARG ZEBRA_SKIP_IPV6_TESTS
ENV ZEBRA_SKIP_IPV6_TESTS ${ZEBRA_SKIP_IPV6_TESTS:-1}

# Use default checkpoint sync and network values if none is provided
ARG CHECKPOINT_SYNC
ENV CHECKPOINT_SYNC ${CHECKPOINT_SYNC:-true}

ARG NETWORK
ENV NETWORK ${NETWORK:-Mainnet}

ENV CARGO_HOME /opt/zebrad/.cargo/

# In this stage we build tests (without running then)
#
# We also download needed dependencies for tests to work, from other images.
# An entrypoint.sh is only available in this step for easier test handling with variables.
FROM deps AS tests
# TODO: do not hardcode the user /root/ even though is a safe assumption
# Pre-download Zcash Sprout, Sapling parameters and Lightwalletd binary
COPY --from=us-docker.pkg.dev/zealous-zebra/zebra/zcash-params /root/.zcash-params /root/.zcash-params
COPY --from=us-docker.pkg.dev/zealous-zebra/zebra/lightwalletd /opt/lightwalletd /usr/local/bin

# Re-hydrate the minimum project skeleton identified by `cargo chef prepare` in the planner stage,
# and build it to cache all possible sentry and test dependencies.
#
# This is the caching Docker layer for Rust!
#
# TODO: is it faster to use --tests here?
RUN cargo chef cook --release --features sentry,lightwalletd-grpc-tests --workspace --recipe-path recipe.json

COPY . .
RUN cargo test --locked --release --features lightwalletd-grpc-tests --workspace --no-run
RUN cp /opt/zebrad/target/release/zebrad /usr/local/bin

COPY ./docker/entrypoint.sh /
RUN chmod u+x /entrypoint.sh

# By default, runs the entrypoint tests specified by the environmental variables (if any are set)
ENTRYPOINT [ "/entrypoint.sh" ]

# In this stage we build a release (generate the zebrad binary)
#
# This step also adds `cargo chef` as this stage is completely independent from the
# `test` stage. This step is a dependency for the `runtime` stage, which uses the resulting
# zebrad binary from this step.
FROM deps AS release
RUN cargo chef cook --release --features sentry --recipe-path recipe.json

COPY . .
# Build zebra
RUN cargo build --locked --release --features sentry --package zebrad --bin zebrad

# This stage is only used when deploying nodes or when only the resulting zebrad binary is needed
#
# To save space, this step starts from scratch using debian, and only adds the resulting
# binary from the `release` stage, and the Zcash Sprout & Sapling parameters from ZCash
FROM debian:bullseye-slim AS runtime
COPY --from=release /opt/zebrad/target/release/zebrad /usr/local/bin
COPY --from=us-docker.pkg.dev/zealous-zebra/zebra/zcash-params /root/.zcash-params /root/.zcash-params

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    ca-certificates

ARG CHECKPOINT_SYNC=true
ARG NETWORK=Mainnet

# Use a configurable dir and file for the zebrad configuration file
ARG ZEBRA_CONF_DIR=/etc/zebra
ENV ZEBRA_CONF_DIR ${ZEBRA_CONF_DIR}

ARG ZEBRA_CONF_FILE=zebrad.toml
ENV ZEBRA_CONF_FILE ${ZEBRA_CONF_FILE}

ARG ZEBRA_CONF_PATH=${ZEBRA_CONF_DIR}/${ZEBRA_CONF_FILE}
ENV ZEBRA_CONF_PATH ${ZEBRA_CONF_PATH}

# Build the `zebrad.toml` before starting the container, using the arguments from build
# time, or using the default values set just above. And create the conf path and file if
# it does not exist.
#
# We disable most ports by default, so the default config is secure.
# Users have to opt-in to additional functionality by editing `zebrad.toml`.
#
# It is safe to use multiple RPC threads in Docker, because we know we are the only running
# `zebrad` or `zcashd` process in the container.
#
# TODO:
#  - move this file creation to an entrypoint as we can use default values at runtime,
#    and modify those as needed when starting the container (at runtime and not at build time)
#  - make `cache_dir`, `rpc.listen_addr`, `metrics.endpoint_addr`, and `tracing.endpoint_addr` into Docker arguments
RUN mkdir -p ${ZEBRA_CONF_DIR} \
    && touch ${ZEBRA_CONF_PATH}
RUN set -ex; \
  { \
    echo "[network]"; \
    echo "network = '${NETWORK}'"; \
    echo "[consensus]"; \
    echo "checkpoint_sync = ${CHECKPOINT_SYNC}"; \
    echo "[state]"; \
    echo "cache_dir = '/zebrad-cache'"; \
    echo "[rpc]"; \
    echo "#listen_addr = '127.0.0.1:8232'"; \
    echo "parallel_cpu_threads = 0"; \
    echo "[metrics]"; \
    echo "#endpoint_addr = '127.0.0.1:9999'"; \
    echo "[tracing]"; \
    echo "#endpoint_addr = '127.0.0.1:3000'"; \
  } > "${ZEBRA_CONF_PATH}"

EXPOSE 8233 18233

ARG SHORT_SHA
ENV SHORT_SHA $SHORT_SHA

ARG SENTRY_DSN
ENV SENTRY_DSN ${SENTRY_DSN}

# TODO: remove the specified config file location and use the default expected by zebrad
CMD zebrad -c "${ZEBRA_CONF_PATH}" start

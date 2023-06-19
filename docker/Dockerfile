# If you want to include a file in the Docker image, add it to .dockerignore.
#
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

# Build zebrad with these features
# Keep these in sync with:
# https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/build-docker-image.yml#L42
ARG FEATURES="sentry"
ARG TEST_FEATURES="lightwalletd-grpc-tests zebra-checkpoints"
# Use ENTRYPOINT_FEATURES to override the specific features used to run tests in entrypoint.test.sh,
# separately from the test and production image builds.
ENV ENTRYPOINT_FEATURES "$TEST_FEATURES $FEATURES"

# Use default network value if none is provided
ARG NETWORK
ENV NETWORK ${NETWORK:-Mainnet}

ENV CARGO_HOME /opt/zebrad/.cargo/

# In this stage we build tests (without running then)
#
# We also download needed dependencies for tests to work, from other images.
# An entrypoint.test.sh is only available in this step for easier test handling with variables.
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
# TODO: add --locked when cargo-chef supports it
RUN cargo chef cook --tests --release --features "${TEST_FEATURES} ${FEATURES}" --workspace --recipe-path recipe.json

COPY . .
# Test Zebra
RUN cargo test --locked --release --features "${TEST_FEATURES} ${FEATURES}" --workspace --no-run
RUN cp /opt/zebrad/target/release/zebrad /usr/local/bin
RUN cp /opt/zebrad/target/release/zebra-checkpoints /usr/local/bin

COPY ./docker/entrypoint.test.sh /
RUN chmod u+x /entrypoint.test.sh

# By default, runs the entrypoint tests specified by the environmental variables (if any are set)
ENTRYPOINT [ "/entrypoint.test.sh" ]

# In this stage we build a release (generate the zebrad binary)
#
# This step also adds `cargo chef` as this stage is completely independent from the
# `test` stage. This step is a dependency for the `runtime` stage, which uses the resulting
# zebrad binary from this step.
FROM deps AS release

# TODO: add --locked when cargo-chef supports it
RUN cargo chef cook --release --features "${FEATURES}" --package zebrad --bin zebrad --recipe-path recipe.json

COPY . .
# Build zebrad
RUN cargo build --locked --release --features "${FEATURES}" --package zebrad --bin zebrad

COPY ./docker/entrypoint.sh /
RUN chmod u+x /entrypoint.sh

# This stage is only used when deploying nodes or when only the resulting zebrad binary is needed
#
# To save space, this step starts from scratch using debian, and only adds the resulting
# binary from the `release` stage, and the Zcash Sprout & Sapling parameters from ZCash
FROM debian:bullseye-slim AS runtime
COPY --from=release /opt/zebrad/target/release/zebrad /usr/local/bin
COPY --from=release /entrypoint.sh /
COPY --from=us-docker.pkg.dev/zealous-zebra/zebra/zcash-params /root/.zcash-params /root/.zcash-params

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    ca-certificates

# Config settings

ARG NETWORK
ENV NETWORK ${NETWORK:-Mainnet}

# Set this to enable the RPC port
ARG RPC_PORT
ENV RPC_PORT ${RPC_PORT}

# Set this to log to a file, if not set, logs to standard output
ARG LOG_FILE
ENV LOG_FILE ${LOG_FILE}

# Set this to change the default cached state directory
ARG ZEBRA_CACHED_STATE_DIR
ENV ZEBRA_CACHED_STATE_DIR ${ZEBRA_CACHED_STATE_DIR:-/var/cache/zebrad-cache}

# Zebra automatically detects if it is attached to a terminal, and uses colored output.
# Set this to 'true' to force using color even if the output is not a terminal.
# Set this to 'false' to disable using color even if the output is a terminal.
ARG LOG_COLOR
ENV LOG_COLOR ${LOG_COLOR}

# Expose configured ports

EXPOSE 8233 18233 $RPC_PORT

# Config location

# Use a configurable dir and file for the zebrad configuration file
ARG ZEBRA_CONF_DIR
ENV ZEBRA_CONF_DIR ${ZEBRA_CONF_DIR:-/etc/zebra}

ARG ZEBRA_CONF_FILE
ENV ZEBRA_CONF_FILE ${ZEBRA_CONF_FILE:-zebrad.toml}

ARG ZEBRA_CONF_PATH
ENV ZEBRA_CONF_PATH ${ZEBRA_CONF_PATH:-$ZEBRA_CONF_DIR/$ZEBRA_CONF_FILE}

# Other settings

ARG SHORT_SHA
ENV SHORT_SHA ${SHORT_SHA}

# Set this to send sentry reports when Zebra crashes
ARG SENTRY_DSN
ENV SENTRY_DSN ${SENTRY_DSN}

# Create a default config file based on the Docker build arguments,
# and report the available zebrad arguments.
# (--help is used as a dummy command.)
RUN /entrypoint.sh --help

# Update the config file based on the Docker run variables,
# and launch zebrad with it
ENTRYPOINT [ "/entrypoint.sh" ]

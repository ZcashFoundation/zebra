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
# We first set default values for build arguments used across the stages.
# Each stage must define the build arguments (ARGs) it uses.
#
# Build zebrad with these features
# Keep these in sync with:
# https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/build-docker-image.yml#L37
ARG FEATURES="default-release-binaries"
ARG TEST_FEATURES="lightwalletd-grpc-tests zebra-checkpoints"

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

# Install zebra build deps and Dockerfile deps
RUN apt-get -qq update && \
    apt-get -qq install -y --no-install-recommends \
    llvm \
    libclang-dev \
    clang \
    ca-certificates \
    protobuf-compiler \
    rsync \
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

# Build arguments and variables set for tracelog levels and debug information
#
# We set defaults to all variables.
ARG RUST_BACKTRACE
ENV RUST_BACKTRACE=${RUST_BACKTRACE:-1}

ARG RUST_LIB_BACKTRACE
ENV RUST_LIB_BACKTRACE=${RUST_LIB_BACKTRACE:-1}

ARG COLORBT_SHOW_HIDDEN
ENV COLORBT_SHOW_HIDDEN=${COLORBT_SHOW_HIDDEN:-1}

ARG SHORT_SHA
# If this is not set, it must be the empty string, so Zebra can try an alternative git commit source:
# https://github.com/ZcashFoundation/zebra/blob/9ebd56092bcdfc1a09062e15a0574c94af37f389/zebrad/src/application.rs#L179-L182
ENV SHORT_SHA=${SHORT_SHA:-}

ENV CARGO_HOME="/opt/zebrad/.cargo/"

# In this stage we build tests (without running then)
#
# We also download needed dependencies for tests to work, from other images.
# An entrypoint.sh is only available in this step for easier test handling with variables.
FROM deps AS tests
# TODO: do not hardcode the user /root/ even though is a safe assumption
# Pre-download Zcash Sprout, Sapling parameters and Lightwalletd binary
COPY --from=us-docker.pkg.dev/zfnd-dev-zebra/zebra/zcash-params:edge /root/.zcash-params /root/.zcash-params
COPY --from=us-docker.pkg.dev/zfnd-dev-zebra/zebra/lightwalletd:edge /opt/lightwalletd /usr/local/bin

# cargo uses timestamps for its cache, so they need to be in this order:
# unmodified source files < previous build cache < modified source files
COPY . .

# Skip IPv6 tests by default, as some CI environment don't have IPv6 available
ARG ZEBRA_SKIP_IPV6_TESTS
ENV ZEBRA_SKIP_IPV6_TESTS=${ZEBRA_SKIP_IPV6_TESTS:-1}

# Use ENTRYPOINT_FEATURES to override the specific features used to run tests in entrypoint.sh,
# separately from the test and production image builds.
ARG FEATURES
ARG TEST_FEATURES
ARG ENTRYPOINT_FEATURES="${FEATURES} ${TEST_FEATURES}"

# Re-hydrate the minimum project skeleton identified by `cargo chef prepare` in the planner stage,
# over the top of the original source files,
# and build it to cache all possible sentry and test dependencies.
#
# This is the caching Docker layer for Rust tests!
# It creates fake empty test binaries so dependencies are built, but Zebra is not fully built.
#
# TODO: add --locked when cargo-chef supports it
RUN cargo chef cook --tests --release --features "${ENTRYPOINT_FEATURES}" --workspace --recipe-path recipe.json

# Undo the source file changes made by cargo-chef.
# rsync invalidates the cargo cache for the changed files only, by updating their timestamps.
# This makes sure the fake empty binaries created by cargo-chef are rebuilt.
COPY --from=planner /opt/zebrad zebra-original
RUN rsync --recursive --checksum --itemize-changes --verbose zebra-original/ .
RUN rm -r zebra-original

# Build Zebra test binaries, but don't run them
RUN cargo test --locked --release --features "${ENTRYPOINT_FEATURES}" --workspace --no-run
RUN cp /opt/zebrad/target/release/zebrad /usr/local/bin
RUN cp /opt/zebrad/target/release/zebra-checkpoints /usr/local/bin

COPY ./docker/entrypoint.sh /
RUN chmod u+x /entrypoint.sh

# Entrypoint environment variables
ENV ENTRYPOINT_FEATURES=${ENTRYPOINT_FEATURES}

# By default, runs the entrypoint tests specified by the environmental variables (if any are set)
ENTRYPOINT [ "/entrypoint.sh" ]

# In this stage we build a release (generate the zebrad binary)
#
# This step also adds `cargo chef` as this stage is completely independent from the
# `test` stage. This step is a dependency for the `runtime` stage, which uses the resulting
# zebrad binary from this step.
FROM deps AS release

COPY . .

ARG FEATURES

# This is the caching layer for Rust zebrad builds.
# It creates a fake empty zebrad binary, see above for details.
#
# TODO: add --locked when cargo-chef supports it
RUN cargo chef cook --release --features "${FEATURES}" --package zebrad --bin zebrad --recipe-path recipe.json

# Undo the source file changes made by cargo-chef, so the fake empty zebrad binary is rebuilt.
COPY --from=planner /opt/zebrad zebra-original
RUN rsync --recursive --checksum --itemize-changes --verbose zebra-original/ .
RUN rm -r zebra-original

# Build zebrad
RUN cargo build --locked --release --features "${FEATURES}" --package zebrad --bin zebrad

COPY ./docker/runtime-entrypoint.sh /
RUN chmod u+x /runtime-entrypoint.sh

# This stage is only used when deploying nodes or when only the resulting zebrad binary is needed
#
# To save space, this step starts from scratch using debian, and only adds the resulting
# binary from the `release` stage, and the Zcash Sprout & Sapling parameters from ZCash
FROM debian:bullseye-slim AS runtime
COPY --from=release /opt/zebrad/target/release/zebrad /usr/local/bin
COPY --from=release /runtime-entrypoint.sh /
COPY --from=us-docker.pkg.dev/zfnd-dev-zebra/zebra/zcash-params:edge /root/.zcash-params /root/.zcash-params

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    ca-certificates

# Config settings for zebrad
ARG FEATURES
ENV FEATURES=${FEATURES}

# Expose configured ports
EXPOSE 8233 18233

# Update the config file based on the Docker run variables,
# and launch zebrad with it
ENTRYPOINT [ "/runtime-entrypoint.sh" ]

# This steps implement cargo-chef for docker layer caching
# We are using four stages:
# - chef: installs cargo-chef
# - planner: computes the recipe file
# - builder: caches our dependencies and builds the binary
# - tester: builds and run tests
# - runtime: is our runtime environment
FROM rust:bullseye as chef
RUN cargo install cargo-chef --locked
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
SHELL ["/bin/bash", "-xo", "pipefail", "-c"]
COPY --from=planner /app/recipe.json recipe.json

# Install zebra build deps
RUN apt-get -qq update && \
    apt-get -qq install -y --no-install-recommends \
    llvm \
    libclang-dev \
    clang \
    ca-certificates \
    cmake \
    protobuf-compiler \
    ; \
    rm -rf /var/lib/apt/lists/* /tmp/*

# Install google OS Config agent
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

ENV CARGO_HOME /app/.cargo/
# Build dependencies - this is the caching Docker layer!
RUN cargo chef cook --release --features enable-sentry --recipe-path recipe.json

ARG RUST_BACKTRACE
ENV RUST_BACKTRACE ${RUST_BACKTRACE:-0}

ARG RUST_LIB_BACKTRACE
ENV RUST_LIB_BACKTRACE ${RUST_LIB_BACKTRACE:-0}

ARG COLORBT_SHOW_HIDDEN
ENV COLORBT_SHOW_HIDDEN ${COLORBT_SHOW_HIDDEN:-0}

# Skip IPv6 tests by default, as some CI environment don't have IPv6 available
ARG ZEBRA_SKIP_IPV6_TESTS
ENV ZEBRA_SKIP_IPV6_TESTS ${ZEBRA_SKIP_IPV6_TESTS:-1}

# Use default checkpoint sync and network values if none is provided
ARG CHECKPOINT_SYNC
ENV CHECKPOINT_SYNC ${CHECKPOINT_SYNC:-true}

ARG NETWORK
ENV NETWORK ${NETWORK:-Mainnet}

COPY . .
# Build zebra
RUN cargo build --locked --release --features enable-sentry --bin zebrad

FROM builder AS tester
# Pre-download Zcash Sprout and Sapling parameters
# TODO: do not hardcode the user /root/ even though is a safe assumption
COPY --from=us-docker.pkg.dev/zealous-zebra/zebra/zcash-params /root/.zcash-params /root/.zcash-params
COPY --from=us-docker.pkg.dev/zealous-zebra/zebra/lightwalletd /lightwalletd /usr/local/bin

RUN cargo test --locked --release --features enable-sentry --workspace --no-run

COPY ./docker/entrypoint.sh /
RUN chmod u+x /entrypoint.sh

ARG CHECKPOINT_SYNC=true
ARG NETWORK=Mainnet
ARG TEST_FULL_SYNC
ENV TEST_FULL_SYNC ${TEST_FULL_SYNC:-0}

ARG RUN_ALL_TESTS
ENV RUN_ALL_TESTS ${RUN_ALL_TESTS:-0}

ENTRYPOINT ["/entrypoint.sh"]
CMD [ "cargo"]

# Runner image
FROM debian:bullseye-slim AS runtime
COPY --from=builder /app/target/release/zebrad /usr/local/bin
COPY --from=us-docker.pkg.dev/zealous-zebra/zebra/zcash-params /root/.zcash-params /root/.zcash-params

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    ca-certificates

RUN set -ex; \
  { \
    echo "[consensus]"; \
    echo "checkpoint_sync = ${CHECKPOINT_SYNC}"; \
    echo "[metrics]"; \
    echo "endpoint_addr = '0.0.0.0:9999'"; \
    echo "[network]"; \
    echo "network = '${NETWORK}'"; \
    echo "[state]"; \
    echo "cache_dir = '/zebrad-cache'"; \
    echo "[tracing]"; \
    echo "endpoint_addr = '0.0.0.0:3000'"; \
  } > "zebrad.toml"

EXPOSE 3000 8233 18233

ARG SHORT_SHA
ENV SHORT_SHA $SHORT_SHA

ARG SENTRY_DSN
ENV SENTRY_DSN ${SENTRY_DSN}

CMD [ "zebrad", "-c", "zebrad.toml", "start" ]

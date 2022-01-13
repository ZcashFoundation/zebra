FROM rust:1.57-buster as builder

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    make cmake g++ gcc llvm libclang-dev clang && \
    rm -Rf /var/lib/apt/lists/* /tmp/*

RUN mkdir /zebra
WORKDIR /zebra

ARG SHORT_SHA
ENV SHORT_SHA $SHORT_SHA

ARG RUST_BACKTRACE
ENV RUST_BACKTRACE ${RUST_BACKTRACE:-full}

# Skip test on debian based OS by default as compilation fails
ARG ZEBRA_SKIP_NETWORK_TESTS
ENV ZEBRA_SKIP_NETWORK_TESTS ${ZEBRA_SKIP_NETWORK_TESTS:-1}

# Optimize builds. In particular, regenerate-stateful-test-disks.yml was reaching the
# GitHub Actions time limit (6 hours), so we needed to make it faster.
ENV RUSTFLAGS -O

ENV CARGO_HOME /zebra/.cargo/

RUN rustc -V; cargo -V; rustup -V

COPY . .

# Pre-download Zcash Sprout and Sapling parameters
RUN cargo run --verbose --bin zebrad download

# Compile, but don't run tests (--no-run). Add verbosity and colors
# Build after compiling tests
RUN cargo test --workspace --no-run
RUN cd zebrad/; cargo build --release --features enable-sentry

# Runner image
FROM debian:buster-slim AS zebrad-release

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    ca-certificates

COPY --from=builder /zebra/target/release/zebrad /

ARG CHECKPOINT_SYNC=true
ARG NETWORK=Mainnet

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
  } > "/zebrad.toml"

RUN cat /zebrad.toml

# Pre-download Zcash Sprout and Sapling parameters
RUN /zebrad download

EXPOSE 3000 8233 18233

ARG RUST_LOG
ENV RUST_LOG ${RUST_LOG:-debug}
ARG RUST_BACKTRACE
ENV RUST_BACKTRACE ${RUST_BACKTRACE:-full}
ARG SENTRY_DSN
ENV SENTRY_DSN ${RUST_BACKTRACE:-https://94059ee72a44420286310990b7c614b5@o485484.ingest.sentry.io/5540918}

CMD [ "/zebrad", "-c", "/zebrad.toml", "start" ]
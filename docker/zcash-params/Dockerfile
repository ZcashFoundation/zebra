# This steps implement cargo-chef for docker layer caching
# This image is for caching Zcash Sprout and Sapling parameters
FROM rust:bullseye as chef
RUN cargo install cargo-chef --locked
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json

# Install zebra build deps
RUN apt-get -qq update && \
    apt-get -qq install -y --no-install-recommends \
    llvm \
    libclang-dev \
    clang \
    ca-certificates \
    ; \
    rm -rf /var/lib/apt/lists/* /tmp/*

# Optimize builds. In particular, regenerate-stateful-test-disks.yml was reaching the
# GitHub Actions time limit (6 hours), so we needed to make it faster.
ENV RUSTFLAGS -O
ENV CARGO_HOME /app/.cargo/
# Build dependencies - this is the caching Docker layer!
RUN cargo chef cook --release --features enable-sentry --recipe-path recipe.json

ARG RUST_BACKTRACE=1
ENV RUST_BACKTRACE ${RUST_BACKTRACE}

COPY . .
# Pre-download Zcash Sprout and Sapling parameters
RUN cargo run --locked --release --features enable-sentry --bin zebrad download

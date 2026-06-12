# syntax=docker/dockerfile:1

ARG UBUNTU_IMAGE=ubuntu:22.04
FROM ${UBUNTU_IMAGE} AS build

ARG DEBIAN_FRONTEND=noninteractive
ARG RUST_VERSION=1.91
ARG FEATURES="default-release-binaries tx_v6"
# Custom rustc cfgs that gate the NU7 + NSM (ZIP-235) consensus paths.
ARG RUSTFLAGS='--cfg zcash_unstable="nu7" --cfg zcash_unstable="zip235"'

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    build-essential \
    ca-certificates \
    clang \
    cmake \
    curl \
    git \
    libclang-dev \
    pkg-config \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

ENV CARGO_HOME=/root/.cargo
ENV RUSTUP_HOME=/root/.rustup
ENV PATH="${CARGO_HOME}/bin:${PATH}"

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
    sh -s -- -y --profile minimal --default-toolchain ${RUST_VERSION}

WORKDIR /workspace

COPY . .

RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/workspace/target \
    RUSTFLAGS="${RUSTFLAGS}" \
    cargo build --locked --release --features "${FEATURES}" --package zebrad --bin zebrad && \
    install -D target/release/zebrad /out/zebra

FROM scratch AS artifact

COPY --from=build /out/zebra /zebra

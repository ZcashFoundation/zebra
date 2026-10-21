# Build image for Anthropic's OSS Scanner (https://red.anthropic.com/oss-scanner/).
#
# The scanner clones this repository, builds this file with the checkout as the
# build context, then audits the result with no network access. Everything the
# audit might need (toolchain, crates, test binaries, a Regtest config) is set
# up here. The enrolment config lives at anthropics/oss-scanner under
# projects/zebra/; .oss-scanner/threat_model.md next to this file tells the
# scanner what Zebra is and how to exercise it.
#
# This file is not used by Zebra's own release or CI images (see docker/).
# BuildKit reads .oss-scanner/Dockerfile.dockerignore instead of the
# repository's .dockerignore, so the whole checkout is copied to /src.

# Keep in sync with rust-toolchain.toml and RUST_VERSION in docker/Dockerfile.
FROM rust:1.91.0-trixie

ENV DEBIAN_FRONTEND=noninteractive

# Zebra's build dependencies, as in docker/Dockerfile: libclang and protoc for
# bindgen and prost, and the system RocksDB so librocksdb-sys links against it
# instead of compiling RocksDB from source. Versions are not pinned: this image
# is rebuilt from `main` by the scanner, not shipped, and the base image tag
# already fixes the Debian release.
# hadolint ignore=DL3008
RUN apt-get update && apt-get install -y --no-install-recommends \
      libclang-dev protobuf-compiler librocksdb-dev clang pkg-config cmake \
      ca-certificates curl git jq \
    && rm -rf /var/lib/apt/lists/*

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

# cargo-nextest: Zebra's test runner. .config/nextest.toml holds the profiles
# and the default filter that excludes the stateful and e2e tiers.
RUN case "$(uname -m)" in \
      x86_64) platform=linux ;; \
      aarch64) platform=linux-arm ;; \
      *) echo "unsupported architecture: $(uname -m)"; exit 1 ;; \
    esac \
    && curl -fsSL "https://get.nexte.st/latest/${platform}" | tar -xzf - -C "${CARGO_HOME}/bin" \
    && cargo nextest --version

# scanner contract: the checkout lives inside the image, at /src
COPY . /src
WORKDIR /src

# Link RocksDB dynamically (faster builds), always show backtraces, and skip
# the IPv6 tests because the sandbox has no IPv6.
ENV ROCKSDB_LIB_DIR=/usr/lib \
    RUST_BACKTRACE=1 \
    CARGO_TERM_COLOR=never \
    CARGO_INCREMENTAL=0 \
    SKIP_IPV6_TESTS=1

# Every crate in Cargo.lock, so later builds with other features or targets
# stay offline.
RUN cargo fetch --locked

# The whole workspace in the dev profile: libraries, the zebrad binary and every
# test binary. Cargo.toml already optimises the cryptographic crates in the dev
# profile, so tests run at a usable speed while Zebra's own code keeps debug
# info, debug assertions and overflow checks. A step that fails fails the build,
# and primary_contact gets the log.
RUN cargo build --locked --workspace --lib --bins \
    && cargo nextest run --locked --workspace --no-run

# Run the fast, network-free part of the suite so the log shows the image works,
# without letting a failure abort the build. zebrad's integration tests and the
# zebra-network tests that dial live peers are left out: they need the Internet.
RUN timeout 900 cargo nextest run --locked --workspace --no-fail-fast \
      -E 'package(zebra-chain) | package(zebra-script) | package(zebra-consensus) | package(zebra-state) | test(/^unit::/)' \
    || echo "WARNING: cargo nextest reported failures"

# Best effort: the libFuzzer harnesses in zebra-fuzz/fuzz are a separate cargo
# workspace that needs nightly and cargo-fuzz. Fetch what they need so they can
# be built offline; nothing here fails the build. The fuzz workspace's own
# Cargo.lock is not kept in step with the node crates, so it is not --locked.
RUN (rustup toolchain install nightly --profile minimal \
      && cargo install --locked cargo-fuzz \
      && cargo +nightly fetch --manifest-path zebra-fuzz/fuzz/Cargo.toml) \
    || echo "WARNING: fuzzing toolchain not installed; cargo fuzz will not work offline"

# A ready-to-use Regtest configuration. Regtest connects to no peers and skips
# Proof of Work, so the `generate`, `getblocktemplate`, `submitblock` and
# `sendrawtransaction` RPCs drive the chain. Setting NU7 to height 1 activates
# it and every earlier network upgrade from the first block, so v5 and v6
# transactions, Orchard and the NU7 rules are all reachable. See
# threat_model.md, "How to exercise it".
RUN mkdir -p /opt/zebra-regtest \
    && printf '%s\n' \
         '[network]' \
         'network = "Regtest"' \
         'listen_addr = "127.0.0.1:18233"' \
         '' \
         '[network.testnet_parameters.activation_heights]' \
         'NU7 = 1' \
         '' \
         '[state]' \
         'ephemeral = true' \
         '' \
         '[rpc]' \
         'listen_addr = "127.0.0.1:18232"' \
         'enable_cookie_auth = false' \
         '' \
         '[mining]' \
         'miner_address = "t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v"' \
         '' \
         '[tracing]' \
         'use_color = false' \
         > /opt/zebra-regtest/zebrad.toml

# Everything is fetched. From here on cargo must not try to reach a registry.
ENV CARGO_NET_OFFLINE=true

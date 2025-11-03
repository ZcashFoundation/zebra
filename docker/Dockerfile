# syntax=docker/dockerfile:1
# check=skip=UndefinedVar,UserExist # We use `runuser` in the entrypoint instead of USER directive

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
ARG GID=${UID}
ARG USER="zebra"
ARG HOME="/home/${USER}"
ARG CARGO_HOME="${HOME}/.cargo"
ARG CARGO_TARGET_DIR="${HOME}/target"
ARG TARGET_ARCH="x86_64-unknown-linux-musl"

FROM stagex/core-busybox@sha256:637b1e0d9866807fac94c22d6dc4b2e1f45c8a5ca1113c88172e0324a30c7283 AS busybox
FROM stagex/core-bash@sha256:6217a843ac51eb8073c3cf13be7d4d1cc9e28f7d7a1f9fd23feb0caa604f73bf AS bash
FROM stagex/core-coreutils@sha256:4f971efaaa5e9d7c5fedbf28a2c1934cb9313c5def51a209c1312cba6edd0973 AS coreutils
FROM stagex/core-openssl@sha256:d6487f0cb15f4ee02b420c717cb9abd85d73043c0bb3a2c6ce07688b23c1df07 AS openssl
FROM stagex/pallet-rust@sha256:9c38bf1066dd9ad1b6a6b584974dd798c2bf798985bf82e58024fbe0515592ca AS pallet-rust
FROM stagex/user-protobuf@sha256:5e67b3d3a7e7e9db9aa8ab516ffa13e54acde5f0b3d4e8638f79880ab16da72c AS protobuf
FROM stagex/user-abseil-cpp@sha256:3dca99adfda0cb631bd3a948a99c2d5f89fab517bda034ce417f222721115aa2 AS abseil-cpp
FROM stagex/core-gcc@sha256:964ffd3793c5a38ca581e9faefd19918c259f1611c4cbf5dc8be612e3a8b72f5 AS gcc
FROM stagex/core-musl@sha256:d9af23284cca2e1002cd53159ada469dfe6d6791814e72d6163c7de18d4ae701 AS musl
FROM stagex/core-libunwind@sha256:eb66122d8fc543f5e2f335bb1616f8c3a471604383e2c0a9df4a8e278505d3bc AS libunwind
FROM stagex/core-user-runtime@sha256:055ae534e1e01259449fb4e0226f035a7474674c7371a136298e8bdac65d90bb AS user-runtime

# This stage prepares Zebra's build deps and captures build args as env vars.
FROM pallet-rust AS deps

# Install zebra build deps
COPY --from=coreutils . /
COPY --from=protobuf . /
COPY --from=abseil-cpp . /
COPY --from=bash . /
COPY --from=busybox . /

SHELL ["/bin/bash", "-xo", "pipefail", "-c"]

# Build arguments and variables
ARG CARGO_INCREMENTAL
ENV CARGO_INCREMENTAL=${CARGO_INCREMENTAL:-0}

ARG CARGO_HOME
ENV CARGO_HOME=${CARGO_HOME}

ARG FEATURES
ENV FEATURES=${FEATURES}

# If this is not set, it must be an empty string, so Zebra can try an
# alternative git commit source:
# https://github.com/ZcashFoundation/zebra/blob/9ebd56092bcdfc1a09062e15a0574c94af37f389/zebrad/src/application.rs#L179-L182
ARG SHORT_SHA
ENV SHORT_SHA=${SHORT_SHA:-}

# This stage builds tests without running them.
#
# We also download needed dependencies for tests to work, from other images.
# An entrypoint.sh is only available in this step for easier test handling with variables.
FROM deps AS tests

# Skip IPv6 tests by default, as some CI environment don't have IPv6 available
ARG ZEBRA_SKIP_IPV6_TESTS
ENV ZEBRA_SKIP_IPV6_TESTS=${ZEBRA_SKIP_IPV6_TESTS:-1}

# This environment setup is almost identical to the `runtime` target so that the
# `tests` target differs minimally. In fact, a subset of this setup is used for
# the `runtime` target.
ARG UID
ENV UID=${UID}
ARG GID
ENV GID=${GID}
ARG USER
ENV USER=${USER}
ARG HOME
ENV HOME=${HOME}

RUN addgroup -g ${GID} ${USER} && \
    adduser -D -H -u ${UID} -G ${USER} -h ${HOME} ${USER} && \
    mkdir -p ${HOME} && \
    chown -R ${UID}:${GID} ${HOME}

# Set the working directory for the build.
WORKDIR ${HOME}

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
    --mount=type=bind,source=tower-batch-control,target=tower-batch-control \
    --mount=type=bind,source=tower-fallback,target=tower-fallback \
    --mount=type=bind,source=Cargo.toml,target=Cargo.toml \
    --mount=type=bind,source=Cargo.lock,target=Cargo.lock \
    --mount=type=cache,target=${HOME}/target/ \
    cargo test --locked --release --workspace --no-run \
    --features "${FEATURES} zebra-checkpoints" && \
    cp ${HOME}/target/release/zebrad /usr/local/bin && \
    cp ${HOME}/target/release/zebra-checkpoints /usr/local/bin

# Copy the lightwalletd binary and source files to be able to run tests
COPY --from=electriccoinco/lightwalletd:v0.4.17 /usr/local/bin/lightwalletd /usr/local/bin/

# Copy the gosu binary to be able to run the entrypoint as non-root user
# and allow to change permissions for mounted cache directories
COPY --from=tianon/gosu:bookworm /gosu /usr/local/bin/

# As the build has already run with the root user,
# we need to set the correct permissions for the home and cargo home dirs owned by it.
RUN chown -R ${UID}:${GID} "${HOME}" && \
    chown -R ${UID}:${GID} "${CARGO_HOME}"

COPY --chown=${UID}:${GID} ./ ${HOME}
COPY --chown=${UID}:${GID} ./docker/entrypoint.sh /usr/local/bin/entrypoint.sh

ENTRYPOINT [ "entrypoint.sh", "test" ]
CMD [ "cargo", "test" ]

# This stage builds the zebrad release binary.
#
# It also adds `cache mounts` as this stage is completely independent from the
# `test` stage. The resulting zebrad binary is used in the `runtime` stage.
FROM deps AS release

# Set the working directory for the build.
ARG HOME
WORKDIR ${HOME}

ARG CARGO_HOME
ARG CARGO_TARGET_DIR
ARG TARGET_ARCH

ENV RUST_BACKTRACE=1
ENV RUSTFLAGS="-C codegen-units=1"
ENV RUSTFLAGS="${RUSTFLAGS} -C target-feature=+crt-static"
ENV RUSTFLAGS="${RUSTFLAGS} -C link-arg=-Wl,--build-id=none"
ENV CFLAGS="-D__GNUC_PREREQ(maj,min)=1"

ENV SOURCE_DATE_EPOCH=1
ENV CXXFLAGS="-include cstdint"
ENV ROCKSDB_USE_PKG_CONFIG=0


RUN --mount=type=bind,source=tower-batch-control,target=tower-batch-control \
    --mount=type=bind,source=tower-fallback,target=tower-fallback \
    --mount=type=bind,source=zebra-chain,target=zebra-chain \
    --mount=type=bind,source=zebra-consensus,target=zebra-consensus \
    --mount=type=bind,source=zebra-network,target=zebra-network \
    --mount=type=bind,source=zebra-node-services,target=zebra-node-services \
    --mount=type=bind,source=zebra-rpc,target=zebra-rpc \
    --mount=type=bind,source=zebra-script,target=zebra-script \
    --mount=type=bind,source=zebra-state,target=zebra-state \
    --mount=type=bind,source=zebra-test,target=zebra-test \
    --mount=type=bind,source=zebra-utils,target=zebra-utils \
    --mount=type=bind,source=zebrad,target=zebrad \
    --mount=type=bind,source=Cargo.toml,target=Cargo.toml \
    --mount=type=bind,source=Cargo.lock,target=Cargo.lock \
    --mount=type=cache,target=${CARGO_TARGET_DIR} \
    --mount=type=cache,target=${CARGO_HOME} \
    cargo fetch --locked --target $TARGET_ARCH && \
    cargo metadata --locked --format-version=1 > /dev/null 2>&1

RUN --network=none \
    --mount=type=bind,source=tower-batch-control,target=tower-batch-control \
    --mount=type=bind,source=tower-fallback,target=tower-fallback \
    --mount=type=bind,source=zebra-chain,target=zebra-chain \
    --mount=type=bind,source=zebra-consensus,target=zebra-consensus \
    --mount=type=bind,source=zebra-network,target=zebra-network \
    --mount=type=bind,source=zebra-node-services,target=zebra-node-services \
    --mount=type=bind,source=zebra-rpc,target=zebra-rpc \
    --mount=type=bind,source=zebra-script,target=zebra-script \
    --mount=type=bind,source=zebra-state,target=zebra-state \
    --mount=type=bind,source=zebra-test,target=zebra-test \
    --mount=type=bind,source=zebra-utils,target=zebra-utils \
    --mount=type=bind,source=zebrad,target=zebrad \
    --mount=type=bind,source=Cargo.toml,target=Cargo.toml \
    --mount=type=bind,source=Cargo.lock,target=Cargo.lock \
    --mount=type=cache,target=${CARGO_TARGET_DIR} \
    --mount=type=cache,target=${CARGO_HOME} \
    cargo build --frozen --release --features "${FEATURES}" --target ${TARGET_ARCH} --package zebrad --bin zebrad && \
    install -D -m 0755 ${HOME}/target/${TARGET_ARCH}/release/zebrad /usr/local/bin/zebrad

# This stage is used for exporting the binary
FROM scratch AS export
COPY --from=release /usr/local/bin/zebrad /zebrad

# This stage starts from scratch using StageX and copies the built zebrad binary
# from the `release` stage along with other binaries and files.
FROM scratch AS runtime
COPY --from=user-runtime . /
COPY --from=busybox . /
COPY --from=bash . /
COPY --from=coreutils . /
COPY --from=openssl . /
COPY --from=gcc  /usr/lib/libgcc_s.so.1 /usr/lib/
COPY --from=gcc  /usr/lib/libstdc++.so.6 /usr/lib/
COPY --from=musl /lib/ld-musl-x86_64.so.1 /lib/
COPY --from=libunwind /lib/libunwind.so.8 /lib/

ARG FEATURES
ENV FEATURES=${FEATURES}

# Create a non-privileged user for running `zebrad`.
#
# We use a high UID/GID (10001) to avoid overlap with host system users.
# This reduces the risk of container user namespace conflicts with host accounts,
# which could potentially lead to privilege escalation if a container escape occurs.
#
# We do not use the `--system` flag for user creation since:
# 1. System user ranges (100-999) can collide with host system users
#   (see: https://github.com/nginxinc/docker-nginx/issues/490)
# 2. There's no value added and warning messages can be raised at build time
#   (see: https://github.com/dotnet/dotnet-docker/issues/4624)
#
# The high UID/GID values provide an additional security boundary in containers
# where user namespaces are shared with the host.
ARG UID
ENV UID=${UID}
ARG GID
ENV GID=${GID}
ARG USER
ENV USER=${USER}
ARG HOME
ENV HOME=${HOME}

RUN addgroup -g ${GID} ${USER} && \
    adduser -D -H -u ${UID} -G ${USER} -h ${HOME} ${USER} && \
    mkdir -p ${HOME} && \
    chown -R ${UID}:${GID} ${HOME}

WORKDIR ${HOME}

# We're explicitly NOT using the USER directive here.
# Instead, we run as root initially and use gosu in the entrypoint.sh
# to step down to the non-privileged user. This allows us to change permissions
# on mounted volumes before running the application as a non-root user.
# User with UID=${UID} is created above and used via gosu in entrypoint.sh.

COPY --from=release /usr/local/bin/zebrad /usr/local/bin/
COPY --chown=${UID}:${GID} ./docker/entrypoint.sh /usr/local/bin/entrypoint.sh

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

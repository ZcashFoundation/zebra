# See here for image contents: https://github.com/microsoft/vscode-dev-containers/tree/v0.245.2/containers/rust/.devcontainer/base.Dockerfile

# [Choice] Debian OS version (use bullseye on local arm64/Apple Silicon): buster, bullseye
ARG VARIANT="bullseye"
FROM mcr.microsoft.com/vscode/devcontainers/rust:0-${VARIANT}

RUN apt-get -qq update && export DEBIAN_FRONTEND=noninteractive && \
    apt-get -qq install -y --no-install-recommends \
    llvm \
    libclang-dev \
    clang \
    ca-certificates \
    protobuf-compiler

RUN cargo install cargo-outdated cargo-audit cargo-edit cargo-diet cargo-about

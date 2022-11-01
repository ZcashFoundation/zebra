# This steps implement cargo-chef for docker layer caching
# This image is for caching Zcash Sprout and Sapling parameters
FROM rust:bullseye as chef
RUN cargo install cargo-chef --locked
WORKDIR /opt/zebrad

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS release
COPY --from=planner /opt/zebrad/recipe.json recipe.json

# Install zebra build deps
RUN apt-get -qq update && \
    apt-get -qq install -y --no-install-recommends \
    llvm \
    libclang-dev \
    clang \
    ca-certificates \
    ; \
    rm -rf /var/lib/apt/lists/* /tmp/*

ENV CARGO_HOME /opt/zebrad/.cargo/
# Build dependencies - this is the caching Docker layer!
RUN cargo chef cook --release --features sentry --package zebrad --recipe-path recipe.json

ARG RUST_BACKTRACE=0
ENV RUST_BACKTRACE ${RUST_BACKTRACE}

ARG RUST_LIB_BACKTRACE=0
ENV RUST_LIB_BACKTRACE ${RUST_LIB_BACKTRACE}

ARG COLORBT_SHOW_HIDDEN=0
ENV COLORBT_SHOW_HIDDEN ${COLORBT_SHOW_HIDDEN}

COPY . .
# Pre-download Zcash Sprout and Sapling parameters
RUN cargo run --locked --release --features sentry --package zebrad --bin zebrad download

FROM rust:buster as builder

RUN apt-get update && \
	apt-get install -y --no-install-recommends \
	make cmake g++ gcc llvm libclang-dev

RUN mkdir /zebra
WORKDIR /zebra

ENV RUST_BACKTRACE 1
ENV CARGO_HOME /zebra/.cargo/

# Copy local code to the container image.
# Assumes that we are in the git repo.
COPY . .
RUN cargo fetch --verbose
COPY . .
RUN rustc -V; cargo -V; rustup -V; cargo test --all && cargo build --release


FROM debian:buster AS zebra-tests

RUN mkdir /zebra
WORKDIR /zebra
COPY --from=builder /zebra/target/debug/zebrad /zebra/target/debug/zebrad
RUN find /zebra/target/debug/deps -type f -perm 755 ! -name '*.dylib' ! -name '*.so' | sed -e 'p;s/-.*//' | xargs -n2 mv
COPY --from=builder /zebra/target/debug/deps/[a-z0-9_]* /zebra/target/debug/deps/
EXPOSE 8233 18233


FROM debian:buster-slim AS zebrad-release

COPY --from=builder /zebra/target/release/zebrad /

RUN printf "[consensus]\n" >> /zebrad.toml
RUN printf "checkpoint_sync = true\n" >> /zebrad.toml
RUN printf "[state]\n" >> /zebrad.toml
RUN printf "cache_dir = '/zebrad-cache'\n" >> /zebrad.toml
RUN printf "memory_cache_bytes = 52428800\n" >> /zebrad.toml
RUN printf "[tracing]\n" >> /zebrad.toml
RUN printf "endpoint_addr = '0.0.0.0:3000'\n" >> /zebrad.toml

EXPOSE 3000 8233 18233

CMD [ "/zebrad", "-c", "/zebrad.toml", "start" ]

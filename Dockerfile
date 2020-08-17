FROM rust:stretch as builder

RUN apt-get update && \
	apt-get install -y --no-install-recommends \
	make cmake g++ gcc llvm clang-format clang-tidy clang-tools clang clangd libc++-dev libc++1 libc++abi-dev libc++abi1 libclang-dev libclang1 liblldb-dev libllvm-ocaml-dev libomp-dev libomp5 lld lldb llvm-dev llvm-runtime llvm python-clang

# libc++-dev

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


FROM debian:buster-slim
COPY --from=builder /zebra/target/release/zebrad /
RUN echo "[tracing]\nendpoint_addr = '0.0.0.0:3000'" > /zebrad.toml
EXPOSE 3000 8233 18233
CMD [ "/zebrad", "start" ]

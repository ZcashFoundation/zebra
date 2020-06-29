FROM rust:buster as builder

RUN apt-get update && \
	apt-get install -y --no-install-recommends \
	make cmake g++ gcc

RUN mkdir /zebra
WORKDIR /zebra

ENV RUST_BACKTRACE 1
ENV CARGO_HOME /zebra/.cargo/

# Copy local code to the container image.
# Assumes that we are in the git repo.
COPY . .
# Hack to tie image hash to workdir/repo state w/o complex build config
RUN sha256sum . | sha256sum > sha256sum.txt
RUN cargo fetch --verbose
COPY . .
RUN rustc -V; cargo -V; rustup -V; cargo test --all && cargo build --release


FROM debian:buster-slim
ENV COMMIT_SHA=${COMMIT_SHA}
COPY --from=builder /zebra/target/release/zebrad /
COPY --from=builder /zebra/sha256sum.txt /
EXPOSE 3000 8233 18233
CMD [ "/zebrad", "start" ]

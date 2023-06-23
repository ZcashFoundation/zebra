# This image is for caching Zcash Sprout and Sapling parameters

FROM debian:bullseye-slim AS release

# Just use the precompiled zebrad binary from a recent test image.
#
# It doesn't matter what build or commit of Zebra we use, because it just calls into the
# zcash_proofs download code. (Which doesn't change much.)
# Release image zebrad binaries would also work.
#
# Compiling the download-params example using `cargo ` is another alternative:
# `cargo run --locked --release --features default-docker --example download-params`
COPY --from=us-docker.pkg.dev/zealous-zebra/zebra/zebrad-test /usr/local/bin/zebrad /usr/local/bin

# Pre-download Zcash Sprout and Sapling parameters
RUN zebrad download

# This image is for caching Zcash Sprout and Sapling parameters.
# We don't test it automatically in CI due to download server rate-limiting.
# To manually run it on the PR branch before merging, go to:
# https://github.com/ZcashFoundation/zebra/actions/workflows/zcash-params.yml

FROM debian:bullseye-slim AS release

# Just use the precompiled zebrad binary from a recent release image.
#
# It doesn't matter what build or commit of Zebra we use, because it just calls into the
# zcash_proofs download code. (Which doesn't change much.)
# Test image zebrad binaries would also work, but it's harder to get a recent tag for them.
#
# Compiling the download-params example using `cargo ` is another alternative:
# `cargo run --locked --release --features default-docker --example download-params`
COPY --from=zfnd/zebra:latest /usr/local/bin/zebrad /usr/local/bin

# Pre-download Zcash Sprout and Sapling parameters
RUN zebrad download

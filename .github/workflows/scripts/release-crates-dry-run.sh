#!/usr/bin/env bash
set -ex

# Check if necessary tools are installed
if ! command -v git &> /dev/null || ! command -v cargo &> /dev/null; then
    echo "ERROR: Required tools (git, cargo) are not installed."
    exit 1
fi

git config --global user.email "release-tests-no-reply@zfnd.org"
git config --global user.name "Automated Release Test"

# Ensure cargo-release is installed
if ! cargo release --version &> /dev/null; then
    echo "ERROR: cargo release must be installed."
    exit 1
fi

# Release process
# Ensure to have an extra `--no-confirm` argument for non-interactive testing.
cargo release version --verbose --execute --no-confirm --allow-branch '*' --workspace --exclude zebrad beta
cargo release version --verbose --execute --no-confirm --allow-branch '*' --package zebrad patch
cargo release replace --verbose --execute --no-confirm --allow-branch '*' --package zebrad
cargo release commit --verbose --execute --no-confirm --allow-branch '*'

# Dry run to check the release
# Workaround for unpublished dependency version errors: https://github.com/crate-ci/cargo-release/issues/691
# TODO: check all crates after fixing these errors
cargo release publish --verbose --dry-run --allow-branch '*' --workspace --exclude zebra-consensus --exclude zebra-utils --exclude zebrad

echo "Release process completed."

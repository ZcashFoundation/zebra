#!/usr/bin/env bash
# Rebuild a zebrad release archive from its tag in an ubuntu:22.04 container
# and compare it with the release's SHA256SUMS. Usage: <script> <version>
#
# The container repeats the "Build and package zebrad" step of
# .github/workflows/zfnd-release-binaries.yml. Keep the two in sync.

set -euo pipefail

VERSION="${1:-}"
VERSION="${VERSION#v}"
if ! [[ "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+[0-9A-Za-z.+-]*$ ]]; then
  echo "usage: $0 <version>, for example $0 7.1.0" >&2
  exit 2
fi

case "$(uname -s)/$(uname -m)" in
  Linux/x86_64) TARGET=x86_64-unknown-linux-gnu ;;
  Linux/aarch64) TARGET=aarch64-unknown-linux-gnu ;;
  *)
    echo "error: needs a Linux host on x86_64 or aarch64" >&2
    exit 2
    ;;
esac
ARCHIVE="zebrad-${VERSION}-${TARGET}.tar.gz"
OUT_DIR="zebrad-rebuild-${VERSION}"

manifest="$(curl --proto '=https' --tlsv1.2 -fsSL \
  "https://github.com/ZcashFoundation/zebra/releases/download/v${VERSION}/SHA256SUMS")"
pattern="${ARCHIVE//./\\.}"
pattern="${pattern//+/\\+}"
expected="$(grep -E "^[0-9a-f]{64}  ${pattern}\$" <<< "${manifest}" | cut -d ' ' -f 1)" || {
  echo "error: SHA256SUMS has no entry for ${ARCHIVE}" >&2
  exit 1
}

mkdir -p "${OUT_DIR}"
# TINI_KILL_PROCESS_GROUP lets Ctrl-C stop cargo, not only the container's shell.
docker run --rm -i --init -e TINI_KILL_PROCESS_GROUP=1 \
  -e VERSION="${VERSION}" -e TARGET="${TARGET}" -e HOME=/home/runner \
  ubuntu:22.04 bash -s > "${OUT_DIR}/${ARCHIVE}" <<'CONTAINER'
set -euo pipefail
# stdout carries only the archive; stdin is closed so no build step reads the rest of this script.
{
export DEBIAN_FRONTEND=noninteractive
apt-get -q update
apt-get -q install -y --no-install-recommends \
  build-essential git ca-certificates curl pkg-config clang libclang-dev cmake protobuf-compiler binutils

# Do not add the rust-src component: it embeds local std paths in the binary.
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
  | sh -s -- -y --profile minimal --default-toolchain none --no-modify-path
export PATH="${HOME}/.cargo/bin:${PATH}"

GITHUB_WORKSPACE=/home/runner/work/zebra/zebra
# zebrad embeds `git describe`, so the clone must contain the release tag.
git clone --quiet --depth 1 --branch "v${VERSION}" \
  https://github.com/ZcashFoundation/zebra.git "${GITHUB_WORKSPACE}"
cd "${GITHUB_WORKSPACE}"
echo "commit: $(git rev-parse HEAD)"
rustup toolchain install
FEATURES=default-release-binaries

SOURCE_DATE_EPOCH="$(git log -1 --pretty=%ct)"
export SOURCE_DATE_EPOCH
export RUSTFLAGS="--remap-path-prefix=${GITHUB_WORKSPACE}=/zebra --remap-path-prefix=${HOME}/.cargo=/cargo"
export CARGO_PROFILE_RELEASE_STRIP=symbols
cargo build --locked --release --features "${FEATURES}" --package zebrad --bin zebrad

stage="zebrad-${VERSION}-${TARGET}"
mkdir -p "dist/${stage}"
cp target/release/zebrad "dist/${stage}/zebrad"
cp LICENSE-APACHE LICENSE-MIT README.md "dist/${stage}/"
tar --sort=name --owner=0 --group=0 --numeric-owner \
  --mtime="@${SOURCE_DATE_EPOCH}" \
  -cf - -C "dist/${stage}" zebrad LICENSE-APACHE LICENSE-MIT README.md \
  | gzip -n > "dist/${stage}.tar.gz"
} < /dev/null >&2
cat "dist/${stage}.tar.gz"
CONTAINER

actual="$(sha256sum < "${OUT_DIR}/${ARCHIVE}" | cut -d ' ' -f 1)"
echo
echo "rebuilt:   ${actual}  ${ARCHIVE}"
echo "published: ${expected}  ${ARCHIVE}"

if [ "${actual}" != "${expected}" ]; then
  echo "The rebuilt archive does not match the published release." >&2
  exit 1
fi
echo "The rebuilt archive matches the published release."

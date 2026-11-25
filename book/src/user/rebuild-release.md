# Rebuilding a Release Binary

You can rebuild a `zebrad` release archive from its git tag on your own machine
and check that it is identical, byte for byte, to the archive published on the
[GitHub release](https://github.com/ZcashFoundation/zebra/releases).

## What a matching rebuild proves

The signature and attestation checks in
[Pre-built binaries](install.md#pre-built-binaries) prove which GitHub workflow
and commit produced an archive. They do not prove that GitHub's build platform
was honest. A matching rebuild checks the platform's output: the published
archive is exactly what the tagged source produces with public tools.

The rebuild still takes the source at the release tag and the `SHA256SUMS`
manifest from GitHub. It does not prove that:

- the tagged source is free of bugs or malicious code,
- the Rust toolchain, the Ubuntu packages, or the crates from crates.io are
  trustworthy, because the rebuild uses the same ones, or
- the [Docker image](docker.md) matches its source.

## Requirements

- A Linux machine on `x86_64` or `aarch64`, with Docker. Only `x86_64`
  rebuilds have been verified so far.
- About 6 GB of free disk space, and network access to GitHub, crates.io, the
  Ubuntu archive, and the rustup servers.

## Rebuild and compare

From a checkout of the Zebra repository, run the script with the release
version. It builds the archive in an `ubuntu:22.04` container with the commands
of the release workflow, and writes it to `zebrad-rebuild-<version>/`:

```bash
git clone https://github.com/ZcashFoundation/zebra.git
cd zebra
scripts/rebuild-release-binary.sh 7.1.0
```

The build takes a few minutes on a large machine. A matching rebuild ends with:

```text
rebuilt:   4b42a23e67fcce27f923882d369dad47f694aa608858f1cbbad70e4298e2ecae  zebrad-7.1.0-x86_64-unknown-linux-gnu.tar.gz
published: 4b42a23e67fcce27f923882d369dad47f694aa608858f1cbbad70e4298e2ecae  zebrad-7.1.0-x86_64-unknown-linux-gnu.tar.gz
The rebuilt archive matches the published release.
```

The script downloads its own copy of `SHA256SUMS` and does not check its
signature. Check that the `published:` digest equals the entry in the
`SHA256SUMS` file whose signature you verified in
[Pre-built binaries](install.md#pre-built-binaries).

The script also prints the commit it built, for example
`commit: 534549fe6b4c851b4785515fd03380cbea9e1a23` for `7.1.0`. To check the
source, compare it with `git rev-parse v7.1.0^{commit}` in another clone.

## Conditions for a matching rebuild

The release build does not pin every input. The script sets up these
conditions:

- **Ubuntu 22.04 tools.** The release's C and C++ dependencies, such as
  RocksDB, are built on an `ubuntu-22.04` runner. The matching `7.1.0` rebuild
  used gcc 11.4.0, clang 14.0.0, glibc 2.35, and binutils 2.38.
- **The Rust toolchain without `rust-src`.** With the `rust-src` component
  installed, Rust embeds local paths to the standard library, and the binary
  differs.
- **The release tag.** `zebrad` embeds the `git describe` output, so the clone
  must contain the tag.

## Verified releases

- `7.1.0` for `x86_64-unknown-linux-gnu`: rebuilt and matched.
- `7.1.0` for `aarch64-unknown-linux-gnu`, and other releases: not verified.

A mismatch on an unverified release can come from a runner image change rather
than tampering. Open an [issue](https://github.com/ZcashFoundation/zebra/issues/new)
with the script output, or follow the
[security policy](https://github.com/ZcashFoundation/zebra/blob/main/SECURITY.md)
if you suspect tampering.

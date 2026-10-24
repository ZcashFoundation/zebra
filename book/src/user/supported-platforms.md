# Platform support

The [platform tier policy](target-tier-policies.md) defines maintenance
commitments; the tables below list Zebra 7.0.0 release targets and CI coverage.
A Rust target triple does not specify a Linux distribution or library versions.

## Minimum supported Rust version (MSRV)

The workspace and library crates require Rust **1.89**. For the daemon's
minimum and pinned toolchain, see
[building from source](install.md#building-from-source).
CI toolchains are listed below.

## Tier 1

The production release targets are 64-bit GNU/Linux on x86 and ARM:

| Target | Release archive build host | Artifacts |
| --- | --- | --- |
| `x86_64-unknown-linux-gnu` | Ubuntu 22.04 | Binary archive and `linux/amd64` image |
| `aarch64-unknown-linux-gnu` | Ubuntu 22.04 ARM | Binary archive and `linux/arm64` image |

See [pre-built binary requirements](install.md#pre-built-binaries) and
[Docker images](docker.md#images-and-tags). The GNU/Linux archives do not run
natively on Alpine Linux.

The [release-binary workflow](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zfnd-release-binaries.yml)
builds both targets natively. The
[Docker workflow](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zfnd-build-docker-image.yml)
builds test images only for AMD64; the main unit-test workflow has no per-change
ARM64 job. See the [Tier 1 policy](target-tier-policies.md#tier-1-platform-policy).

## Tier 2

The additional CI environment, x86-64 Ubuntu 26.04, runs workspace and crate
builds, minimum-version checks, Clippy, and unit tests. It produces no separate
Ubuntu-specific release archive.

| Target | Environment | Toolchain coverage |
| --- | --- | --- |
| `x86_64-unknown-linux-gnu` | GitHub Actions `ubuntu-26.04` | Rust 1.97.0 builds and tests; Rust 1.91.0 daemon build; Rust 1.89 library-crate builds |

The target triple is the same as the x86-64 Tier 1 release target; the tier
here describes an additional OS/toolchain environment. See the
[Tier 2 policy](target-tier-policies.md#tier-2-platform-policy).

Scheduled or manually triggered
[advisory jobs](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/advisory.yml)
run Clippy and unit tests with beta Rust on Ubuntu 26.04. They do not guarantee
that each change builds with beta.

## Tier 3

The following platforms have no official `zebrad` release archives and no
full-node build-and-test guarantee:

| Target | Platform | CI limitation |
| --- | --- | --- |
| `x86_64-apple-darwin` | macOS on Intel | No macOS build or test job |
| `aarch64-apple-darwin` | macOS on Apple silicon | No macOS build or test job |
| `x86_64-pc-windows-msvc` | Windows with the MSVC toolchain | CI tests only the `zebra-test` helper crate, not `zebrad` |

Build from source on macOS; use a supported Linux environment for production.
The zcashd-compat preflight supports Linux only, independently of these
native-node tiers. See the [Tier 3 policy](target-tier-policies.md#tier-3-platform-policy).

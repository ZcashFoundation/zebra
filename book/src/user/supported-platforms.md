# Platform Support

Support for different platforms are organized into three tiers, each with a
different set of guarantees. For more information on the policies for platforms
at each tier, see the [Platform Tier Policy](target-tier-policies.md).

Platforms are identified by their Rust "target triple" which is a string composed by
`<machine>-<vendor>-<operating system>`.

## Tier 1

Tier 1 platforms can be thought of as "guaranteed to work". The Zebra project
builds official binary releases for each tier 1 platform, and automated testing
ensures that each tier 1 platform builds and passes tests after each change.

For the full requirements, see [Tier 1 platform policy](target-tier-policies.md#tier-1-platform-policy) in the Platform Tier Policy.

| platform | os | notes | rust | artifacts
| -------|-------|-------|-------|-------
| `x86_64-unknown-linux-gnu` | [Debian 13](https://www.debian.org/releases/trixie/) | 64-bit | [latest stable release](https://github.com/rust-lang/rust/releases) | Docker
| `aarch64-unknown-linux-gnu` | [Debian 13](https://www.debian.org/releases/trixie/) | 64-bit ARM64 | [latest stable release](https://github.com/rust-lang/rust/releases) | Docker

## Tier 2

Tier 2 platforms can be thought of as "guaranteed to build". The Zebra project
builds in CI for each tier 2 platform, and automated builds ensure that each
tier 2 platform builds after each change. Not all automated tests are run so it's
not guaranteed to produce a working build, and official builds are not available,
but tier 2 platforms often work to quite a good degree and patches are always
welcome!

For the full requirements, see [Tier 2 platform policy](target-tier-policies.md#tier-2-platform-policy) in the Platform Tier Policy.

| platform | os | notes | rust | artifacts
| -------|-------|-------|-------|-------
| `x86_64-unknown-linux-gnu` | [GitHub ubuntu-latest](https://github.com/actions/virtual-environments#available-environments) | 64-bit | [latest stable release](https://github.com/rust-lang/rust/releases) | N/A
| `x86_64-unknown-linux-gnu` | [GitHub ubuntu-latest](https://github.com/actions/virtual-environments#available-environments) | 64-bit | [latest beta release](https://github.com/rust-lang/rust/blob/beta/src/version) | N/A
| `x86_64-apple-darwin` | [GitHub macos-latest](https://github.com/actions/virtual-environments#available-environments) | 64-bit | [latest stable release](https://github.com/rust-lang/rust/releases) | N/A
| `x86_64-pc-windows-msvc` | [GitHub windows-latest](https://github.com/actions/virtual-environments#available-environments) | 64-bit | [latest stable release](https://github.com/rust-lang/rust/releases) | N/A

## Tier 3

Tier 3 platforms are those which the Zebra codebase has support for, but which
the Zebra project does not build or test automatically, so they may or may not
work. Official builds are not available.

For the full requirements, see [Tier 3 platform policy](target-tier-policies.md#tier-3-platform-policy) in the Platform Tier Policy.

| platform | os | notes | rust | artifacts
| -------|-------|-------|-------|-------
| `aarch64-apple-darwin` | latest macOS | 64-bit, Apple M1 or M2 | [latest stable release](https://github.com/rust-lang/rust/releases) | N/A

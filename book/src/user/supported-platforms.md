# Platform Support

Support for different platforms ("targets") are organized into three tiers,
each with a different set of guarantees. For more information on the policies
for targets at each tier, see the [Target Tier Policy](platform-tier-policy.md).
## Tier 1

Tier 1 targets can be thought of as "guaranteed to work". The Zebra project
builds official binary releases for each tier 1 platform, and automated testing
ensures that each tier 1 platform builds and passes tests after each change.

For the full requirements, see [Tier 1 platform policy](platform-tier-policy.md#tier-1-platform-policy) in the Target Tier Policy.

platform | notes
-------|-------
`x86_64-unknown-linux-gnu` | 64-bit Linux (kernel 4.19+, glibc 2.33+)

## Tier 2

Tier 2 targets can be thought of as "guaranteed to build". The Zebra project
builds in CI for each tier 2 platform, and automated builds ensure that each
tier 2 platform builds after each change. Not all automated tests are run so it's
not guaranteed to produce a working build, but tier 2 targets often work to
quite a good degree and patches are always welcome!

For the full requirements, see [Tier 2 platform policy](platform-tier-policy.md#tier-2-platform-policy) in the Target Tier Policy.

platform | notes
-------|-------
`x86_64-apple-darwin` | 64-bit macOS (12.4, darwin 21.5.0)

## Tier 3

Tier 3 targets are those which the Zebra codebase has support for, but which the
Zebra project does not build or test automatically, so they may or may not work.
Official builds are not available.

For the full requirements, see [Tier 3 platform policy](platform-tier-policy.md#tier-3-platform-policy) in the Target Tier
Policy.

platform | notes
-------|-------
`aarch64-unknown-linux-gnu` | ARM64 Linux (kernel 4.2, glibc 2.33+)
`i686-unknown-linux-gnu` | 32-bit Linux (kernel 4.19+, glibc 2.33+)

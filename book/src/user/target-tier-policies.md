# Platform tier policy

## General

Zebra's platform tiers define maintenance commitments, following the model of
the [Rust target tier policy](https://doc.rust-lang.org/stable/rustc/target-tier-policy.html).
A platform includes its architecture, operating system, toolchain, and required
native libraries. The [platform support matrix](supported-platforms.md) lists
the release targets and the CI coverage for Zebra 7.0.0.

The Zebra team approves additions, promotions, demotions, and removals.
Each tier inherits the requirements of the tier below it. A platform must
first be accepted at the lower tier and demonstrate sustained maintenance
before promotion; the Zebra team determines the required period. Meeting a
checklist does not automatically confer a tier.

The words _must_ and _must not_ express requirements. The words _should_ and
_should not_ express requirements for which the Zebra team can approve an
exception. The word _may_ expresses permission. This terminology follows
[RFC 2119](https://www.rfc-editor.org/rfc/rfc2119).

## Tier 3 platform policy

Tier 3 offers no build, test, or release guarantee. Other contributors need not
maintain or test its platform-specific support.

- A platform must have identified maintainers who can receive reports of
  platform-specific issues.
- Target names should identify the actual environment without ambiguity.
- Changes must not break Tier 1 or Tier 2 platforms. A known regression on
  another Tier 3 platform requires agreement from that platform's maintainers
  or the Zebra team.
- Tier 3 failures must not block unrelated pull requests. Automated failure
  notifications must go only to people who have opted into them.
- Support must not impose a disproportionate maintenance burden on the rest
  of Zebra.

The Zebra team may remove an inactive or unmaintained Tier 3 platform, or one
whose support impedes the codebase. A removal proposal must notify the
platform maintainers so they can respond.

## Tier 2 platform policy

Tier 2 requires reliable builds in CI. A platform-specific build failure in a
required job blocks acceptance of the affected change. Tier 2 does not promise
that every test passes or that the project publishes binaries for that
platform.

The DevOps team must approve the platform's CI integration.

- The platform must implement Zcash consensus rules. The Zebra team may
  approve excluding other features or development tools.
- At least one designated developer must maintain the platform and respond to
  platform-specific build failures.
- The platform must be a Rust Tier 1 target and have a usable Rust toolchain.
- Documentation must specify CPU and OS baselines, native dependencies,
  build commands, and test commands. It must identify required hardware or
  emulation when an ordinary development host cannot run the target.
- Required CI builds must be reliable. Their duration and infrastructure cost
  must not impose an excessive burden on the project.
- Platform maintainers should run the test suite regularly and fix failures.
  The Zebra team may require selected tests as part of the platform's CI
  contract.

Contributors need not become platform experts. The Zebra team handles test
failures according to their impact: it may require a fix, disable a test, or
demote the platform.

## Tier 1 platform policy

Tier 1 adds production support and release artifacts to the Tier 2
requirements. The Zebra team, release team, and DevOps team must approve a
promotion and the infrastructure needed to sustain it.

- The platform must support the standard production feature set: peer-to-peer
  networking, consensus verification, the mempool, persistent state, and
  RPC services. The Zebra team must approve exceptions.
- The platform must provide adequate security, performance, and reliability
  for production use, and serve users across multiple organizations or
  projects.
- Required builds and tests must pass reliably. The platform must not achieve
  this by disabling an excessive part of the test suite.
- Building and running tests must not take substantially longer than on other
  supported platforms.
- Release artifacts must have a documented runtime baseline and a maintained
  build and publication path.
- Maintainers must provide any additional hardware, cloud capacity, or
  emulation needed for CI and platform-specific debugging. CI resources must
  be continuously available for the project's exclusive use; debugging
  resources must be exclusive while in use.
- Emulation used for required checks must accurately represent native
  behavior. Discrepancies affecting test results must be treated as
  high-priority platform, compiler, or emulator defects.

The Zebra team decides whether a failure requires a fix before other changes
merge, a fix before release, a test exception, or a tier change. Changes to a
Tier 1 CPU or OS baseline require team approval and communication to users.

Tier 1 or Tier 2 demotion or removal proposals must notify maintainers and the
Zcash community before a stable release. Notice depends on severity and urgency.
Changes normally apply to development and future releases, not retroactively.

# Platform Tier Policy

## Table of Contents

- [Platform Tier Policy](#platform-tier-policy)
  - [Table of Contents](#table-of-contents)
  - [General](#general)
  - [Tier 3 platform policy](#tier-3-platform-policy)
  - [Tier 2 platform policy](#tier-2-platform-policy)
  - [Tier 1 platform policy](#tier-1-platform-policy)

## General

The Zcash Foundation provides three tiers of platform support, modeled after the
[Rust Target Tier Policy](https://doc.rust-lang.org/stable/rustc/target-tier-policy.html):

- The Zcash Foundation provides no guarantees about tier 3 platforms; they may
  or may not build with the actual codebase.
- Zebra's continuous integration checks that tier 2 platforms will always build,
  but they may or may not pass tests.
- Zebra's continuous integration checks that tier 1 platforms will always build
  and pass tests.

Adding a new tier 3 platform imposes minimal requirements; but we focus
primarily on avoiding disruption to ongoing Zebra development.

Tier 2 and tier 1 platforms place work on Zcash Foundation developers as a whole,
to avoid breaking the platform. These tiers require commensurate and ongoing
efforts from the maintainers of the platform, to demonstrate value and to
minimize any disruptions to ongoing Zebra development.

This policy defines the requirements for accepting a proposed platform at a
given level of support.

Each tier is based on all the requirements from the previous tier, unless
overridden by a stronger requirement.

While these criteria attempt to document the policy, that policy still involves
human judgment. Targets must fulfill the spirit of the requirements as well, as
determined by the judgment of the Zebra team.

For a list of all supported platforms and their corresponding tiers ("tier 3",
"tier 2", or "tier 1"), see
[platform support](supported-platforms.md).

Note that a platform must have already received approval for the next lower
tier, and spent a reasonable amount of time at that tier, before making a
proposal for promotion to the next higher tier; this is true even if a platform
meets the requirements for several tiers at once. This policy leaves the
precise interpretation of "reasonable amount of time" up to the Zebra team.

The availability or tier of a platform in stable Zebra is not a hard stability
guarantee about the future availability or tier of that platform. Higher-level
platform tiers are an increasing commitment to the support of a platform, and
we will take that commitment and potential disruptions into account when
evaluating the potential demotion or removal of a platform that has been part
of a stable release. The promotion or demotion of a platform will not generally
affect existing stable releases, only current development and future releases.

In this policy, the words "must" and "must not" specify absolute requirements
that a platform must meet to qualify for a tier. The words "should" and "should
not" specify requirements that apply in almost all cases, but for which the
Zebra team may grant an exception for good reason. The word "may" indicates
something entirely optional, and does not indicate guidance or recommendations.
This language is based on [IETF RFC 2119](https://tools.ietf.org/html/rfc2119).

## Tier 3 platform policy

At this tier, the Zebra project provides no official support for a platform, so
we place minimal requirements on the introduction of platforms.

- A tier 3 platform must have a designated developer or developers (the "platform
  maintainers") on record to be CCed when issues arise regarding the platform.
  (The mechanism to track and CC such developers may evolve over time.)
- Target names should not introduce undue confusion or ambiguity unless
  absolutely necessary to maintain ecosystem compatibility. For example, if
  the name of the platform makes people extremely likely to form incorrect
  beliefs about what it targets, the name should be changed or augmented to
  disambiguate it.
- Tier 3 platforms must not impose burden on the authors of pull requests, or
  other developers in the community, to maintain the platform. In particular,
  do not post comments (automated or manual) on a PR that derail or suggest a
  block on the PR based on a tier 3 platform. Do not send automated messages or
  notifications (via any medium, including via `@`) to a PR author or others
  involved with a PR regarding a tier 3 platform, unless they have opted into
  such messages.
- Patches adding or updating tier 3 platforms must not break any existing tier 2
  or tier 1 platform, and must not knowingly break another tier 3 platform
  without approval of either the Zebra team of the other tier 3 platform.

If a tier 3 platform stops meeting these requirements, or the platform maintainers
no longer have interest or time, or the platform shows no signs of activity and
has not built for some time, or removing the platform would improve the quality
of the Zebra codebase, we may post a PR to remove support for that platform. Any such PR will be CCed
to the platform maintainers (and potentially other people who have previously
worked on the platform), to check potential interest in improving the situation.

## Tier 2 platform policy

At this tier, the Zebra project guarantees that a platform builds, and will reject
patches that fail to build on a platform. Thus, we place requirements that ensure
the platform will not block forward progress of the Zebra project.

A proposed new tier 2 platform must be reviewed and approved by Zebra team based
on these requirements.

In addition, the devops team must approve the integration of the platform
into Continuous Integration (CI), and the tier 2 CI-related requirements. This
review and approval may take place in a PR adding the platform to CI, or simply
by a devops team member reporting the outcome of a team discussion.

- Tier 2 platforms must implement all the Zcash consensus rules.
  Other Zebra features and binaries may be disabled, on a case-by-case basis.
- A tier 2 platform must have a designated team of developers (the "platform
  maintainers") available to consult on platform-specific build-breaking issues.
  This team must have at least 1 developer.
- The platform must not place undue burden on Zebra developers not specifically
  concerned with that platform. Zebra developers are expected to not gratuitously
  break a tier 2 platform, but are not expected to become experts in every tier 2
  platform, and are not expected to provide platform-specific implementations for
  every tier 2 platform.
- The platform must provide documentation for the Zcash community explaining how
  to build for their platform, and explaining how to run tests for the platform.
  If at all possible, this documentation should show how to run Zebra programs
  and tests for the platform using emulation, to allow anyone to do so. If the
  platform cannot be feasibly emulated, the documentation should document the
  required physical hardware or cloud systems.
- The platform must document its baseline expectations for the features or
  versions of CPUs, operating systems, and any other dependencies.
- The platform must build reliably in CI, for all components that Zebra's CI
  considers mandatory.
  - Since a working Rust compiler is required to build Zebra,
    the platform must be a [Rust tier 1 platform](https://rust-lang.github.io/rustup-components-history/).
- The Zebra team may additionally require that a subset of tests pass in
  CI. In particular, this requirement may apply if the tests in question provide
  substantial value via early detection of critical problems.
- Building the platform in CI must not take substantially longer than the current
  slowest platform in CI, and should not substantially raise the maintenance
  burden of the CI infrastructure. This requirement is subjective, to be
  evaluated by the devops team, and will take the community importance
  of the platform into account.
- Test failures on tier 2 platforms will be handled on a case-by-case basis.
  Depending on the severity of the failure, the Zebra team may decide to:
  - disable the test on that platform,
  - require a fix before the next release, or
  - remove the platform from tier 2.
- The platform maintainers should regularly run the testsuite for the platform,
  and should fix any test failures in a reasonably timely fashion.
- All requirements for tier 3 apply.

A tier 2 platform may be demoted or removed if it no longer meets these
requirements. Any proposal for demotion or removal will be CCed to the platform
maintainers, and will be communicated widely to the Zcash community before being
dropped from a stable release. (The amount of time between such communication
and the next stable release may depend on the nature and severity of the failed
requirement, the timing of its discovery, whether the platform has been part of a
stable release yet, and whether the demotion or removal can be a planned and
scheduled action.)

## Tier 1 platform policy

At this tier, the Zebra project guarantees that a platform builds and passes all
tests, and will reject patches that fail to build or pass the testsuite on a
platform. We hold tier 1 platforms to our highest standard of requirements.

A proposed new tier 1 platform must be reviewed and approved by the Zebra team
based on these requirements. In addition, the release team must approve the
viability and value of supporting the platform.

In addition, the devops team must approve the integration of the platform
into Continuous Integration (CI), and the tier 1 CI-related requirements. This
review and approval may take place in a PR adding the platform to CI, by a
devops team member reporting the outcome of a team discussion.

- Tier 1 platforms must implement Zebra's standard production feature set,
  including the network protocol, mempool, cached state, and RPCs.
  Exceptions may be made on a case-by-case basis.
  - Zebra must have reasonable security, performance, and robustness on that platform.
    These requirements are subjective, and determined by consensus of the Zebra team.
  - Internal developer tools and manual testing tools may be disabled for that platform.
- The platform must serve the ongoing needs of multiple production
  users of Zebra across multiple organizations or projects. These requirements
  are subjective, and determined by consensus of the Zebra team. A tier 1
  platform may be demoted or removed if it becomes obsolete or no longer meets
  this requirement.
- The platform must build and pass tests reliably in CI, for all components that
  Zebra's CI considers mandatory.
  - Test failures on tier 1 platforms will be handled on a case-by-case basis.
    Depending on the severity of the failure, the Zebra team may decide to:
    - disable the test on that platform,
    - require a fix before the next release,
    - require a fix before any other PRs merge, or
    - remove the platform from tier 1.
  - The platform must not disable an excessive number of tests or pieces of tests
    in the testsuite in order to do so. This is a subjective requirement.
- Building the platform and running the testsuite for the platform must not take
  substantially longer than other platforms, and should not substantially raise
  the maintenance burden of the CI infrastructure.
  - In particular, if building the platform takes a reasonable amount of time,
    but the platform cannot run the testsuite in a timely fashion due to low
    performance, that alone may prevent the platform from qualifying as tier 1.
- If running the testsuite requires additional infrastructure (such as physical
  systems running the platform), the platform maintainers must arrange to provide
  such resources to the Zebra project, to the satisfaction and approval of the
  Zebra devops team.
  - Such resources may be provided via cloud systems, via emulation, or via
    physical hardware.
  - If the platform requires the use of emulation to meet any of the tier
    requirements, the Zebra team must have high
    confidence in the accuracy of the emulation, such that discrepancies
    between emulation and native operation that affect test results will
    constitute a high-priority bug in either the emulation, the Rust
    implementation of the platform, or the Zebra implementation for the platform.
  - If it is not possible to run the platform via emulation, these resources
    must additionally be sufficient for the Zebra devops team to make them
    available for access by Zebra team members, for the purposes of development
    and testing. (Note that the responsibility for doing platform-specific
    development to keep the platform well maintained remains with the platform
    maintainers. This requirement ensures that it is possible for other
    Zebra developers to test the platform, but does not obligate other Zebra
    developers to make platform-specific fixes.)
  - Resources provided for CI and similar infrastructure must be available for
    continuous exclusive use by the Zebra project. Resources provided
    for access by Zebra team members for development and testing must be
    available on an exclusive basis when in use, but need not be available on a
    continuous basis when not in use.
- All requirements for tier 2 apply.

A tier 1 platform may be demoted if it no longer meets these requirements but
still meets the requirements for a lower tier. Any such proposal will be
communicated widely to the Zcash community, both when initially proposed and
before being dropped from a stable release. A tier 1 platform is highly unlikely
to be directly removed without first being demoted to tier 2 or tier 3.
(The amount of time between such communication and the next stable release may
depend on the nature and severity of the failed requirement, the timing of its
discovery, whether the platform has been part of a stable release yet, and
whether the demotion or removal can be a planned and scheduled action.)

Raising the baseline expectations of a tier 1 platform (such as the minimum CPU
features or OS version required) requires the approval of the Zebra team, and
should be widely communicated as well.

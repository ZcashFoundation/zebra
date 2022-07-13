# Target Tier Policy

## Table of Contents

- [Target Tier Policy](#target-tier-policy)
  - [Table of Contents](#table-of-contents)
  - [General](#general)
  - [Tier 3 target policy](#tier-3-target-policy)
  - [Tier 2 target policy](#tier-2-target-policy)
  - [Tier 1 target policy](#tier-1-target-policy)

## General

Zebra provides three tiers of target support:

- Zebra provides no guarantees about tier 3 targets; they exist in the codebase,
  but may or may not build.
- Zebra's continuous integration checks that tier 2 targets will always build,
  but they may or may not pass tests.
- Zebra's continuous integration checks that tier 1 targets will always build
  and pass tests.

Adding a new tier 3 target imposes minimal requirements; but we focus primarily
on avoiding disruption to other ongoing Zebra development.

Tier 2 and tier 1 targets place work on Zebra project developers as a whole, to
avoid breaking the target. These tiers require commensurate and ongoing efforts
from the maintainers of the target, to demonstrate value and to minimize any
disruptions to ongoing Zebra development.

This policy defines the requirements for accepting a proposed target at a given
level of support.

Each tier is based on all the requirements from the previous tier, unless
overridden by a stronger requirement.

While these criteria attempt to document the policy, that policy still involves
human judgment. Targets must fulfill the spirit of the requirements as well, as
determined by the judgment of the approving teams. Neither this policy nor any
decisions made regarding targets shall create any binding agreement or estoppel
by any party.

For a list of all supported targets and their corresponding tiers ("tier 3",
"tier 2", or "tier 1"), see
[platform support](platform-support.md).

Note that a target must have already received approval for the next lower tier,
and spent a reasonable amount of time at that tier, before making a proposal
for promotion to the next higher tier; this is true even if a target meets the
requirements for several tiers at once. This policy leaves the precise
interpretation of "reasonable amount of time" up to the approving teams; those
teams may scale the amount of time required based on their confidence in the
target and its demonstrated track record at its current tier. At a minimum,
multiple stable releases of Zebra should typically occur between promotions of a
target.

The availability or tier of a target in stable Zebra is not a hard stability
guarantee about the future availability or tier of that target. Higher-level
target tiers are an increasing commitment to the support of a target, and we
will take that commitment and potential disruptions into account when
evaluating the potential demotion or removal of a target that has been part of
a stable release. The promotion or demotion of a target will not generally
affect existing stable releases, only current development and future releases.

In this policy, the words "must" and "must not" specify absolute requirements
that a target must meet to qualify for a tier. The words "should" and "should
not" specify requirements that apply in almost all cases, but for which the
approving teams may grant an exception for good reason. The word "may"
indicates something entirely optional, and does not indicate guidance or
recommendations. This language is based on [IETF RFC
2119](https://tools.ietf.org/html/rfc2119).

## Tier 3 target policy

At this tier, the Zebra project provides no official support for a target, so
we place minimal requirements on the introduction of targets.

- A tier 3 target must have a designated developer or developers (the "target
  maintainers") on record to be CCed when issues arise regarding the target.
  (The mechanism to track and CC such developers may evolve over time.)
- Targets must use naming consistent with any existing targets; for instance, a
  target for the same CPU or OS as an existing Zebra target should use the same
  name for that CPU or OS. Targets should normally use the same names and
  naming conventions as used elsewhere in the broader ecosystem beyond Zebra
  (such as in other toolchains), unless they have a very good reason to
  diverge. Changing the name of a target can be highly disruptive, especially
  once the target reaches a higher tier, so getting the name right is important
  even for a tier 3 target.
  - Target names should not introduce undue confusion or ambiguity unless
    absolutely necessary to maintain ecosystem compatibility. For example, if
    the name of the target makes people extremely likely to form incorrect
    beliefs about what it targets, the name should be changed or augmented to
    disambiguate it.
- Neither this policy nor any decisions made regarding targets shall create any
  binding agreement or estoppel by any party. If any member of an approving
  Zebra team serves as one of the maintainers of a target, or has any legal or
  employment requirement (explicit or implicit) that might affect their
  decisions regarding a target, they must recuse themselves from any approval
  decisions regarding the target's tier status, though they may otherwise
  participate in discussions.
- The target must provide documentation for the Zebra community explaining how
  to build for the target, using cross-compilation if possible. If the target
  supports running binaries, or running tests (even if they do not pass), the
  documentation must explain how to run such binaries or tests for the target,
  using emulation if possible or dedicated hardware if necessary.
- Tier 3 targets must not impose burden on the authors of pull requests, or
  other developers in the community, to maintain the target. In particular,
  do not post comments (automated or manual) on a PR that derail or suggest a
  block on the PR based on a tier 3 target. Do not send automated messages or
  notifications (via any medium, including via `@`) to a PR author or others
  involved with a PR regarding a tier 3 target, unless they have opted into
  such messages.
- Patches adding or updating tier 3 targets must not break any existing tier 2
  or tier 1 target, and must not knowingly break another tier 3 target without
  approval of either the core engineering team or the maintainers of the other
  tier 3 target.

If a tier 3 target stops meeting these requirements, or the target maintainers
no longer have interest or time, or the target shows no signs of activity and
has not built for some time, or removing the target would improve the quality
of the Zebra codebase, we may post a PR to remove it; any such PR will be CCed
to the target maintainers (and potentially other people who have previously
worked on the target), to check potential interest in improving the situation.

## Tier 2 target policy

At this tier, the Zebra project guarantees that a target builds, and will reject
patches that fail to build on a target. Thus, we place requirements that ensure
the target will not block forward progress of the Zebra project.

A proposed new tier 2 target must be reviewed and approved by the core
engineering team based on these requirements.

In addition, the devops team must approve the integration of the target
into Continuous Integration (CI), and the tier 2 CI-related requirements. This
review and approval may take place in a PR adding the target to CI, or simply
by a devops team member reporting the outcome of a team discussion.

- A tier 2 target must have value to people other than its maintainers. (It may
  still be a niche target, but it must not be exclusively useful for an
  inherently closed group.)
- A tier 2 target must have a designated team of developers (the "target
  maintainers") available to consult on target-specific build-breaking issues,
  or if necessary to develop target-specific language or library implementation
  details. This team must have at least 1 developer.
- The target must not place undue burden on Zebra developers not specifically
  concerned with that target. Zebra developers are expected to not gratuitously
  break a tier 2 target, but are not expected to become experts in every tier 2
  target, and are not expected to provide target-specific implementations for
  every tier 2 target.
- The target must provide documentation for the Zebra community explaining how
  to build for the target using cross-compilation, and explaining how to run
  tests for the target. If at all possible, this documentation should show how
  to run Zebra programs and tests for the target using emulation, to allow
  anyone to do so. If the target cannot be feasibly emulated, the documentation
  should explain how to obtain and work with physical hardware, cloud systems,
  or equivalent.
- The target must document its baseline expectations for the features or
  versions of CPUs, operating systems, libraries, runtime environments, and
  similar.
- Tier 2 targets must not leave any significant portions of `zebra` unimplemented
  or stubbed out, unless they cannot possibly be
  supported on the target.
  - The right approach to handling a missing feature from a target may depend
    on whether the target seems likely to develop the feature in the future. In
    some cases, a target may be co-developed along with Zebra support, and Zebra
    may gain new features on the target as that target gains the capabilities
    to support those features.
  - As an exception, a target identical to an existing tier 1 target except for
    lower baseline expectations for the OS, CPU, or similar, may propose to
    qualify as tier 2 (but not higher) without support for `std` if the target
    will primarily be used in `no_std` applications, to reduce the support
    burden for the standard library. In this case, evaluation of the proposed
    target's value will take this limitation into account.
- The target must build reliably in CI, for all components that Zebra's CI
  considers mandatory.
- The approving teams may additionally require that a subset of tests pass in
  CI, such as enough to build a functional "hello world" program, `./x.py test
  --no-run`, or equivalent "smoke tests". In particular, this requirement may
  apply if the tests in question provide substantial value via early detection
  of critical problems.
- Building the target in CI must not take substantially longer than the current
  slowest target in CI, and should not substantially raise the maintenance
  burden of the CI infrastructure. This requirement is subjective, to be
  evaluated by the devops team, and will take the community importance
  of the target into account.
- Tier 2 targets should, if at all possible, support cross-compiling. Tier 2
  targets should not require using the target as the host for builds, even if
  the target supports host tools.
- Tier 2 targets must not impose burden on the authors of pull requests, or
  other developers in the community, to ensure that tests pass for the target.
  In particular, do not post comments (automated or manual) on a PR that derail
  or suggest a block on the PR based on tests failing for the target. Do not
  send automated messages or notifications (via any medium, including via `@`)
  to a PR author or others involved with a PR regarding the PR breaking tests
  on a tier 2 target, unless they have opted into such messages.
- The target maintainers should regularly run the testsuite for the target, and
  should fix any test failures in a reasonably timely fashion.
- All requirements for tier 3 apply.

A tier 2 target may be demoted or removed if it no longer meets these
requirements. Any proposal for demotion or removal will be CCed to the target
maintainers, and will be communicated widely to the Zebra community before being
dropped from a stable release. (The amount of time between such communication
and the next stable release may depend on the nature and severity of the failed
requirement, the timing of its discovery, whether the target has been part of a
stable release yet, and whether the demotion or removal can be a planned and
scheduled action.)

## Tier 1 target policy

At this tier, the Zebra project guarantees that a target builds and passes all
tests, and will reject patches that fail to build or pass the testsuite on a
target. We hold tier 1 targets to our highest standard of requirements.

A proposed new tier 1 target must be reviewed and approved by the core
engineering team based on these requirements. In addition, the release team must
approve the viability and value of supporting the target.

In addition, the devops team must approve the integration of the target
into Continuous Integration (CI), and the tier 1 CI-related requirements. This
review and approval may take place in a PR adding the target to CI, by a devops
team member reporting the outcome of a team discussion.

- Tier 1 targets must have substantial, widespread interest within the
  developer community, and must serve the ongoing needs of multiple production
  users of Zebra across multiple organizations or projects. These requirements
  are subjective, and determined by consensus of the approving teams. A tier 1
  target may be demoted or removed if it becomes obsolete or no longer meets
  this requirement.
- The target maintainer team must include at least 2 developers.
- The target must build and pass tests reliably in CI, for all components that
  Zebra's CI considers mandatory.
  - The target must not disable an excessive number of tests or pieces of tests
    in the testsuite in order to do so. This is a subjective requirement.
- Building the target and running the testsuite for the target must not take
  substantially longer than other targets, and should not substantially raise
  the maintenance burden of the CI infrastructure.
  - In particular, if building the target takes a reasonable amount of time,
    but the target cannot run the testsuite in a timely fashion due to low
    performance of either native code or accurate emulation, that alone may
    prevent the target from qualifying as tier 1.
- If running the testsuite requires additional infrastructure (such as physical
  systems running the target), the target maintainers must arrange to provide
  such resources to the Zebra project, to the satisfaction and approval of the
  Zebra devops team.
  - Such resources may be provided via cloud systems, via emulation, or via
    physical hardware.
  - If the target requires the use of emulation to meet any of the tier
    requirements, the approving teams for those requirements must have high
    confidence in the accuracy of the emulation, such that discrepancies
    between emulation and native operation that affect test results will
    constitute a high-priority bug in either the emulation or the
    implementation of the target.
  - If it is not possible to run the target via emulation, these resources must
    additionally be sufficient for the Zebra devops team to make them
    available for access by Zebra team members, for the purposes of development
    and testing. (Note that the responsibility for doing target-specific
    development to keep the target well maintained remains with the target
    maintainers. This requirement ensures that it is possible for other
    Zebra developers to test the target, but does not obligate other Zebra
    developers to make target-specific fixes.)
  - Resources provided for CI and similar infrastructure must be available for
    continuous exclusive use by the Zebra project. Resources provided
    for access by Zebra team members for development and testing must be
    available on an exclusive basis when in use, but need not be available on a
    continuous basis when not in use.
- All requirements for tier 2 apply.

A tier 1 target may be demoted if it no longer meets these requirements but
still meets the requirements for a lower tier. Any such proposal will be
communicated widely to the Zebra community, both when initially proposed and
before being dropped from a stable release. A tier 1 target is highly unlikely
to be directly removed without first being demoted to tier 2 or tier 3.
(The amount of time between such communication and the next stable release may
depend on the nature and severity of the failed requirement, the timing of its
discovery, whether the target has been part of a stable release yet, and whether
the demotion or removal can be a planned and scheduled action.)

Raising the baseline expectations of a tier 1 target (such as the minimum CPU
features or OS version required) requires the approval of the core engineering
team and release teams, and should be widely communicated as well.

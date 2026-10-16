---
status: accepted
date: 2026-10-16
builds-on: "[Release binary artifacts and supply-chain posture](0008-release-binary-artifacts.md)"
story: How a published Zebra version receives a patch after `main` has moved on
---

# Release branches for patch releases

## Context and Problem Statement

The release pipeline plans, publishes, and deploys from `main`, so each release ships whatever `main` holds when its Release PR merges.

Operators on the current stable release sometimes need one bug fixed after `main` has moved on to new features or a network upgrade. The project needs a way to publish the released version plus the fix, and nothing else.

## Priorities & Constraints

- `zebrad` is a consensus node. A patch must be the smallest possible change over the version operators already run.
- Each release halts at its end-of-support height (`EOS_PANIC_AFTER` in `zebrad/src/components/sync/end_of_support.rs`). A line that stops receiving patches stops running.
- Every fix must reach `main`, or it disappears at the next feature release.
- Patch volume is low, a few per year, and maintainer attention is the scarce resource. The process must add no standing machinery.
- One publication pipeline: the Release PR, changelog batching, `ZcashFoundation/cargo-release`, binaries, images, and deploys behave the same whatever branch they run from.
- Code on a release branch ships to production, so it needs the same review and required checks as `main`.
- Default installs and the production fleet follow "latest" markers, so a patch for an older line must not take them.

## Considered Options

1. Fix on `main` only, and ask operators to upgrade to the next feature release.
2. Cut a release branch at every release.
3. Create `release/X.Y` when a line first needs a patch, develop the fix on that branch, and forward-port it to `main`.
4. Develop every fix on `main` first, and backport it onto the release branch.
5. Release from `main` with the unwanted changes reverted.

### Pros and Cons of the Options

#### Option 1: Fix on `main` only

- Good, because it is the process already in place.
- Bad, because the upgrade it asks for is the change the operator is trying to avoid, such as a network upgrade or an API break.
- Bad, because an urgent fix waits for whatever else `main` has half-finished.

#### Option 2: A release branch at every release

- Good, because the branch already exists when a patch is needed.
- Bad, because most branches never receive a commit, yet each one consumes protection, CI, and dependency-bot attention.

#### Option 3: A `release/X.Y` branch created on demand, fixed first, forward-ported to `main`

- Good, because a branch exists only where support work exists.
- Good, because the fix is reviewed and tested against the tree operators run, so the diff that ships is the diff that was reviewed.
- Good, because a patch never waits for `main` to be releasable.
- Bad, because each fix costs two reviews, one on the branch and one on the forward-port.
- Bad, because a forward-port nobody opens leaves the fix on the branch only.

#### Option 4: Fix on `main` first, then backport

- Good, because `main` is always ahead, so a fix cannot be lost.
- Bad, because the diff that ships was reviewed against `main`, not against the tree it is applied to.
- Bad, because the patch waits until the fix is mergeable on `main`, and conflicts surface on the release branch under time pressure.

#### Option 5: Release from `main` with reverts

- Good, because no new branch or protection rule is needed.
- Bad, because the revert set grows with every change on `main`, and the published tree is one nobody has ever run.

## Decision Outcome

Chosen option 3, because it is the only option where the diff that ships is the diff reviewed against the tree operators run, without standing branches.

1. **Patch the current stable line.** That is the line of the latest published stable release, whatever version an open Release PR on `main` proposes. Supporting an older line takes a separate, explicit decision.
2. **Create `release/X.Y` on demand,** from the newest tag on that line. Release branches are protected like `main`: the same review and required checks, and no deletion or force-push.
3. **Develop the fix on the release branch.** The fix pull request targets `release/X.Y`. Cherry-picking a fix that already merged to `main` is a shortcut, not a requirement.
4. **Land only reviewed, release-specific changes on a release branch:** the fix, the release metadata, and the pipeline files the release needs. Never merge `main` into a release branch.
5. **Carry every fix into `main`** through a forward-port pull request that is tracked until it merges. The next feature release must include every applicable released fix. No backport automation is adopted.
6. **Publish through the same Release PR pipeline as `main`.** A maintainer merges the Release PR on the branch, and publication is automated from there.
7. **Docker `latest` and the production deploy follow GitHub's Latest marker,** the highest published stable version, checked immediately before either one changes. A patch that is not Latest still publishes its crates, tag, GitHub Release, binaries, and Docker `X.Y.Z` tag.
8. **Retire a release branch only by an explicit support decision,** not because `main` proposes a newer version or the end-of-support height was renewed. What the branch published stays published.

### Expected Consequences

- Each fix costs two reviews, one on the branch and one on the forward-port.
- A release candidate is never Latest, so publishing one does not deploy production.
- Decisions 6 and 7 depend on release pipeline work tracked in [#11433](https://github.com/ZcashFoundation/zebra/issues/11433).

## More Information

- Release process, patch releases section: [`book/src/dev/release-process.md`](../../../book/src/dev/release-process.md#patch-releases-from-a-release-branch)
- GitHub REST, `make_latest` and the latest release: <https://docs.github.com/rest/releases/releases>

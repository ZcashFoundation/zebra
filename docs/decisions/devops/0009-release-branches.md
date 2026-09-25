---
status: accepted
date: 2026-09-17
builds-on: "[Release binary artifacts and supply-chain posture](0008-release-binary-artifacts.md)"
story: How a published Zebra version receives a patch after `main` has moved on
---

# Release branches for patch releases

## Context and Problem Statement

Every Zebra release so far is a linear descendant of the release before it. The release pipeline plans, publishes, and deploys from `main`, so each release ships whatever `main` holds at merge time.

Operators running the current stable release sometimes need one bug fixed. By then `main` has moved on. It carries new features, and at times a network upgrade. Taking a fix from `main` means taking all of that as well. The project needs a place to build the next patch that is the published release plus the fix, and nothing else.

## Priorities & Constraints

- `zebrad` is a consensus node. A patch must be the smallest possible change over the version the operator already runs.
- Each release halts at its end-of-support height. `EOS_PANIC_AFTER` is 105 days after `ESTIMATED_RELEASE_HEIGHT` in `zebrad/src/components/sync/end_of_support.rs`. A version that stops receiving patches stops running.
- Every fix must reach `main`. A fix that exists only on a release branch disappears at the next feature release.
- Network-upgrade parameters belong to the code, not to the support window. A patch that keeps a node running does not make it follow the right chain.
- Patch volume is low, a few per year. Maintainer attention is the scarce resource, so the process must add no standing machinery to operate.
- One publication pipeline. The Release PR, changie batching, `ZcashFoundation/cargo-release` reconciliation, crates, tags, binaries, images, and deploys have to behave the same whatever branch they run from.
- Latest markers decide what operators install by default. GitHub's `make_latest` and docker/metadata-action's `latest` flavor both default to "most recently published", so a patch for an older version would claim both unless the pipeline guards them.
- Code on a release branch ships to production. It needs the same review and the same required checks as `main`.

## Considered Options

1. Fix on `main` only, and ask operators to upgrade to the next feature release.
2. Cut a release branch at every release.
3. Create `release/X.Y` when a line first needs a patch, develop the fix on that branch, and forward-port it to `main`.
4. Develop every fix on `main` first, and require a backport onto the release branch.
5. Release from `main` with the unwanted changes reverted.

### Pros and Cons of the Options

#### Option 1: Fix on `main` only

- Good, because it is the process already in place, with nothing new to learn or protect.
- Good, because there is only ever one supported line of development.
- Bad, because the upgrade it asks for is the change the operator is trying to avoid. A node that needs one bug fixed has to take a network upgrade or an API break at the same time.
- Bad, because it couples patch urgency to the readiness of `main`. An urgent fix waits for whatever else `main` has half-finished.

#### Option 2: A release branch at every release

- Good, because the branch always exists when a patch is needed, with no setup step.
- Bad, because most branches never receive a commit. Each one still consumes protection rules, CI matrix entries, and dependency-bot traffic.
- Bad, because unused branches invite confusion about which line is current.

#### Option 3: A lazily created `release/X.Y`, developed stable-first, forward-ported to `main`

- Good, because a branch exists only where a patch exists. Branch count matches real support work.
- Good, because the fix is written, reviewed, and tested against the tree operators actually run. The diff that ships is the diff that was reviewed.
- Good, because a patch never waits for `main` to be releasable.
- Good, because reusing a fix that already exists on `main` stays available as a shortcut. `git cherry-pick -x -m 1 <merge commit>` takes the whole reviewed change as one commit and records its source, so provenance is readable in `git log`.
- Good, because the forward-port is adapted through review when `main` has changed, which is where that adaptation belongs.
- Bad, because each fix costs two reviews, one on the branch and one on the forward-port.
- Bad, because a forward-port nobody opens leaves the fix on the branch only. It has to be tracked.

#### Option 4: Main-first with mandatory backports

- Good, because `main` is always ahead, so a fix can never be lost.
- Good, because the code is reviewed once and the backport is meant to be mechanical.
- Bad, because the diff that ships is not the diff that was reviewed. The change is reviewed against `main` and then applied to an older tree.
- Bad, because it blocks the patch on `main` being mergeable. While `main` is mid-upgrade, the patch waits.
- Bad, because conflicts surface on the release branch under time pressure, at the point where the change has had the least review.

#### Option 5: Release from `main` with reverts

- Good, because no new branch or protection rule is needed.
- Bad, because the revert set grows with every change on `main` and has to be recomputed for each patch.
- Bad, because the published version is not the audited release plus one fix. It is a tree nobody has ever run.
- Bad, because the reverts either pollute `main`'s history or live on a throwaway branch, which is a release branch without the protection.

## Decision Outcome

Chosen option 3: a `release/X.Y` branch created when a line first needs a patch, the fix developed on that branch, and a tracked forward-port pull request into `main`.

**1. Patches are developed stable-first.** A hotfix is developed against the current stable release line. The fix pull request targets `release/X.Y` directly, so the change is reviewed and tested against the tree operators run. Reusing a fix that already exists on `main` is an optional shortcut, never a prerequisite: `git cherry-pick -x -m 1 <merge commit>` brings the whole reviewed change over as one commit and records where it came from. Every fix must eventually reach `main` through a tracked forward-port pull request, adapted through review when `main` has changed.

**2. The current stable line receives the patch.** That is the line of the latest published stable release. It does not depend on the version an unmerged Release PR on `main` proposes. Supporting an older line takes a separate explicit decision.

**3. The branch is created on demand, from the latest published stable release on its line.** `release/X.Y` is an ordinary public branch. A maintainer creates it when the line first needs a patch, starting from the newest tag on that line: `release/6.3` from `v6.3.1` when that is the newest 6.3 tag, and from `v6.3.0` otherwise. The `Release branches` ruleset covers `refs/heads/release/**` with the same requirements as `PR Requirements` on `main`, plus deletion and force-push protection. It does not include a merge queue: GitHub rejects merge queues on wildcard refs. A per-line merge-queue ruleset is an optional later admin step once a concrete `release/X.Y` exists.

**4. Three kinds of change land on a release branch, each one reviewed.** The fix; the release metadata, meaning versions, changelog entries, and `ESTIMATED_RELEASE_HEIGHT` in `zebrad/src/components/sync/end_of_support.rs`; and the build or workflow infrastructure the release needs. When the branch's starting tag predates changes to the release pipeline, the first pull request is a narrowly reviewed infrastructure update: copy the required scripts, workflow files, and configuration, and initialize changelog history only through that stable release. Never copy `main`'s whole `.changes/unreleased/` directory. Never replace all of `.github/` wholesale. Never merge `main` into the release branch. Each of these changes must be visible in review as part of the release.

**5. The next feature release from `main` must carry the fixes.** It has to include every applicable released fix and the correct network-upgrade parameters. Renewing the end-of-support height on a patch does not by itself establish network compatibility.

**6. A release branch is retired only after an explicit support decision.** Do not derive retirement from the version `main` proposes, and do not derive it from a renewed end-of-support height. The tags, releases, crates, and images the branch published stay where they are.

**7. The Release PR on a release branch works as it does on `main`.** release-plz opens `chore: release vX.Y.Z` against the release branch, a maintainer reviews and merges it, and publication is automated from there. Release PR head branches are namespaced per base branch, `release-plz-main-*` on `main` and `release-plz-6.3-*` on `release/6.3`, because release-plz deduplicates open Release PRs by head-branch prefix across all base branches. Recovery uses the same dispatch, pointed at the branch:

```sh
gh workflow run release.yml --ref release/X.Y \
  -f operation=check|resume \
  -f release_pr_number=<PR>
```

**8. Latest and production follow GitHub's latest-release marker.** `ZcashFoundation/cargo-release` decides whether a release is marked Latest on GitHub. The binaries and deploy workflows always publish the immutable `X.Y.Z` artifacts, then recheck that marker immediately before mutating Docker Hub `latest` or deploying production. A patch that is not marked Latest still publishes crates, tags, the GitHub Release, signed binaries, and the Docker Hub `X.Y.Z` tag.

**9. No new automation.** There is no backport bot, no scheduled job that copies commits between branches, and no second release engine. Pull requests are opened by hand and run through the pipeline that already exists.

### Expected Consequences

- A published version can receive a fix without its operators taking anything else. The patch tree is the released tag plus the fix plus the release metadata.
- The diff that ships is the diff that was reviewed, against the tree the operator runs.
- A patch never waits for `main` to be releasable, so an urgent fix is not blocked by unfinished work elsewhere.
- Each fix costs two reviews, and the forward-port has to be tracked. A forward-port that is never opened loses the fix at the next feature release.
- Branch count tracks support work. A branch exists only while its line is supported, and retiring it is a decision someone makes.
- Operators pulling `zfnd/zebra:latest` and the production fleet follow GitHub's latest-release marker, so a patch on an older line publishes underneath them.

### Gated future work

**Backport automation.** Once many fixes move between `main` and a release branch, a label-driven bot repays its setup. Revisit it then with the credential question answered first: the bot must not be able to push to a protected branch that ships to production without a reviewed pull request.

## More Information

- Release process, patch releases section: [`book/src/dev/release-process.md`](../../../book/src/dev/release-process.md)
- release-plz `pr_branch_prefix`: <https://release-plz.dev/docs/config#the-pr_branch_prefix-field>
- GitHub REST, create a release and `make_latest`: <https://docs.github.com/rest/releases/releases#create-a-release>
- docker/metadata-action `flavor` input: <https://github.com/docker/metadata-action#flavor-input>
- `git cherry-pick`, the `-x` and `-m` options: <https://git-scm.com/docs/git-cherry-pick>

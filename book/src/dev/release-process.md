# Zebra versioning and releases

This document contains the practices that we follow to provide you with a leading-edge application, balanced with stability.
We strive to ensure that future changes are always introduced in a predictable way.
We want everyone who depends on Zebra to know when and how new features are added, and to be well-prepared when obsolete ones are removed.

Before reading, you should understand [Semantic Versioning](https://semver.org/spec/v2.0.0.html) and how a [Trunk-based development](https://www.atlassian.com/continuous-delivery/continuous-integration/trunk-based-development) works

<a id="versioning"></a>

## Zebra versioning

Zebra version numbers show the impact of the changes in a release. They are composed of three parts: `major.minor.patch`.
For example, version `3.1.11` indicates major version 3, minor version 1, and patch level 11.

The version number is incremented based on the level of change included in the release.

| Level of change | Details                                                                                                                                                                                                                                                              |
| :-------------- | :------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Major release   | Contains significant new features, and commonly correspond to network upgrades; some technical assistance may be needed during the update. When updating to a major release, you may need to follow the specific upgrade instructions provided in the release notes. |
| Minor release   | Contains new smaller features. Minor releases should be fully backward-compatible. No technical assistance is expected during update. If you want to use the new features in a minor release, you might need to follow the instructions in the release notes.        |
| Patch release   | Low risk, bug fix release. No technical assistance is expected during update.                                                                                                                                                                                        |

<a id="supported-releases"></a>

### Supported Releases

Every Zebra version released by the Zcash Foundation is supported up to a specific height. Currently we support each version for about **15 weeks** (see `EOS_PANIC_AFTER` in `zebrad/src/components/sync/end_of_support.rs`) but this can change from release to release.

When the Zcash chain reaches this end of support height, `zebrad` will shut down and the binary will refuse to start.

The current stable release line can receive a bug fix as a patch release, without waiting for the next minor or major release. See [Patch releases from a release branch](#patch-releases-from-a-release-branch).

Our process is similar to `zcashd`: <https://zcash.github.io/zcash/user/release-support.html>

Older Zebra versions that only support previous network upgrades will never be supported, because they are operating on an unsupported Zcash chain fork.

<a id="updating"></a>

### Supported update paths

You can update to any version of Zebra, provided that the following criteria are met:

- The version you want to update _to_ is supported.
- The version you want to update _from_ is within one major version of the version you want to upgrade to.

<a id="previews"></a>

### Preview releases

We let you preview what's coming by providing Release Candidate \(`rc`\) pre-releases for some major releases:

| Pre-release type  | Details                                                                                                                                                                |
| :---------------- | :--------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Beta              | The release that is under active development and testing. The beta release is indicated by a release tag appended with the `-beta` identifier, such as `8.1.0-beta.0`. |
| Release candidate | A release for final testing of new features. A release candidate is indicated by a release tag appended with the `-rc` identifier, such as version `8.1.0-rc.0`.       |

### Distribution tags

Zebra's tagging relates directly to versions published on Docker. We will reference these [Docker Hub distribution tags](https://hub.docker.com/r/zfnd/zebra/tags) throughout:

| Tag    | Description                                                                                         |
| :----- | :-------------------------------------------------------------------------------------------------- |
| latest | The most recent stable version.                                                                     |
| beta   | The most recent pre-release version of Zebra for testing. May not always exist.                     |
| rc     | The most recent release candidate of Zebra, meant to become a stable version. May not always exist. |

### Feature Flags

To keep the `main` branch in a releasable state, experimental features must be gated behind a [Rust feature flag](https://doc.rust-lang.org/cargo/reference/features.html).
Breaking changes should also be gated behind a feature flag, unless the team decides they are urgent.
(For example, security fixes which also break backwards compatibility.)

<a id="frequency"></a>

## Release frequency

We work toward a regular schedule of releases, so that you can plan and coordinate your updates with the continuing evolution of Zebra.

<div class="alert is-helpful">

Dates are offered as general guidance and are subject to change.

</div>

In general, expect the following release cycle:

- A major release for each network upgrade, whenever there are breaking changes to Zebra (by API, severe bugs or other kind of upgrades)
- Minor releases for significant new Zebra features or severe bug fixes
- A patch release around every 6 weeks

This cadence of releases gives eager developers access to new features as soon as they are fully developed and pass through our code review and integration testing processes, while maintaining the stability and reliability of the platform for production users that prefer to receive features after they have been validated by Zcash and other developers that use the pre-release builds.

<a id="deprecation"></a>

## Deprecation practices

Sometimes "breaking changes", such as the removal of support for RPCs, APIs, and features, are necessary to:

- add new Zebra features,
- improve Zebra performance or reliability,
- stay current with changing dependencies, or
- implement changes in the \(blockchain\) itself.

To make these transitions as straightforward as possible, we make these commitments to you:

- We work hard to minimize the number of breaking changes and to provide migration tools, when possible
- We follow the deprecation policy described here, so you have time to update your applications to the latest Zebra binaries, RPCs and APIs
- If a feature has critical security or reliability issues, and we need to remove it as soon as possible, we will explain why at the top of the release notes

To help ensure that you have sufficient time and a clear path to update, this is our deprecation policy:

| Deprecation stages | Details                                                                                                                                                                                                                                                                                                                                                                                |
| :----------------- | :------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Announcement       | We announce deprecated RPCs and features in the [change log](https://github.com/ZcashFoundation/zebra/blob/main/CHANGELOG.md "Zebra change log"). When we announce a deprecation, we also announce a recommended update path.                                                                                                                                                          |
| Deprecation period | When a RPC or a feature is deprecated, it is still present until the next major release. A deprecation can be announced in any release, but the removal of a deprecated RPC or feature happens only in major release. Until a deprecated RPC or feature is removed, it is maintained according to the Tier 1 support policy, meaning that only critical and security issues are fixed. |
| Rust APIs          | The Rust APIs of the Zebra crates are currently unstable and unsupported. Use the `zebrad` commands or JSON-RPCs to interact with Zebra.                                                                                                                                                                                                                                               |

<a id="process"></a>

## Release candidate & release process

The normal release path requires 2 maintainer actions:

1. Review the latest Release PR after every required check passes.
2. Approve and merge the latest commit.

Everything else is automatic. release-plz creates and updates a PR whose branch starts with `release-plz-` and carries the `release` label, `PR Gate / Release readiness` validates it, and `ZcashFoundation/cargo-release` publishes from that PR's source range after merge.

### Review the Release PR

Wait until release-plz finishes updating the PR and every required check passes, then review the latest commit and complete every checkbox in its generated checklist. Each checked box records that a maintainer performed that validation; for a conditional item, check it after validating the condition or confirming that it does not apply. Checklist edits use the standard PR Gate workflow, so wait for the latest run before approval. Source PRs commit curated change fragments under `.changes/unreleased/`, then the Release workflow batches them into versioned entries for the versions release-plz picked and regenerates every changelog, writing a mechanical dependency entry for a package that is being released only because a local dependency moved. Before approval, any required checkpoint, end-of-support height, README, or operational release-note changes must land on the base branch.

A new Release PR commit replaces the generated body and resets every checkbox. Treat only the latest checklist and required-check results as authoritative.

Approve and merge only after every required check passes and every checkbox is complete. A later release-plz update invalidates the earlier review and checklist.

### What Release Readiness Reports

Every new Release PR commit automatically runs `PR Gate / Release readiness`. The job confirms that the PR includes the current base branch, validates each changed package's versioned changelog, and runs Cargo 1.91's multi-package dry-run. Changelog and Cargo validation run independently, so the summary reports both outcomes even when one fails. The job also observes crates.io, tags, and the GitHub Release without changing them.

Before publication, a green report with `reason: "incomplete"` is expected: the plan and dry-run passed, while the planned crates, tags, or GitHub Release are correctly absent. The job summary shows the complete plan and observed state, so maintainers can review readiness without running local commands.

### What Happens After Merge

Merging the approved Release PR starts the post-merge release workflow. The controller uses the merge commit as the immutable release source and its first parent as the publication range base. Cargo Release publishes missing crates in dependency order, verifies that the published `zebrad` installs when it is part of the plan, then creates missing tags and the public `zebrad` GitHub Release. The release commit does not create another Release PR. That GitHub Release triggers the downstream workflows that publish signed Docker images, attach signed and checksummed Linux `x86_64` and `aarch64` binaries, and deploy long-lived GCP nodes.

No maintainer command is required when this workflow succeeds.

> [!IMPORTANT]
> Before the first release using this controller, a crates.io owner must confirm [Trusted Publishing](https://crates.io/docs/trusted-publishing) for every publishable workspace crate with repository `ZcashFoundation/zebra`, workflow `release.yml`, and environment `release`. Trusted Publisher bindings are visible only to crate owners, so repository CI cannot verify this prerequisite.

### If Release Readiness Fails

Open the failed job summary before retrying. Each failure identifies the next action:

| Failure | Next action |
| --- | --- |
| The Release PR is behind `main` | Wait for release-plz to update the PR. |
| A versioned changelog is missing or empty | For a direct package change, add the missing fragment on `main` with `changie new -j <project>`, then let release-plz refresh the PR. A dependency-only failure indicates a changelog batching regression; do not edit the generated branch. |
| Cargo's dry-run fails | Fix the source or dependency problem on `main`. |
| Crate provenance, a tag target, or a release channel conflicts | Stop and ask a maintainer to investigate. |

Each new Release PR commit reruns the complete readiness check.

If no readiness run is available, first rerun the PR checks in GitHub. The manual dispatch below is the readiness exit hatch when the automatic PR check still does not start:

```sh
gh workflow run release.yml --ref main \
  -f operation=check \
  -f release_pr_number=<PR>
```

### If Publication Stops After Merge

Resume the same merged Release PR:

```sh
gh workflow run release.yml --ref main \
  -f operation=resume \
  -f release_pr_number=<PR>
```

`resume` reuses the same Release PR source, skips matching crates and tags, and continues from missing external state. It can repair configured mutable GitHub Release metadata, such as the release name, notes, draft state, or latest selection.

Do not retry immutable contradictions: a crate archive from another source commit, a tag that points to another commit, or a GitHub Release with a conflicting channel must stop for maintainer review. Do not manually repeat publication or overwrite external state.

If the automated workflow remains unavailable and a maintainer authorizes a break-glass manual release, follow the [manual release checklist](https://github.com/ZcashFoundation/zebra/blob/main/.github/PULL_REQUEST_TEMPLATE/release-checklist-legacy.md).

## Patch releases from a release branch

A patch release ships a published version plus a fix, and nothing else. It is built on a `release/X.Y` branch instead of `main`, so operators take the fix without taking anything else that has landed since.

The reasoning behind this process is recorded in [ADR 0009](https://github.com/ZcashFoundation/zebra/blob/main/docs/decisions/devops/0009-release-branches.md).

### Which line receives patches

Patches go to the current stable release line, the line of the latest published stable release. It does not matter which version an unmerged Release PR on `main` proposes. If 6.3.1 is the latest published stable release, the patch target is `release/6.3`.

Supporting an older line is a separate decision that maintainers make explicitly. Do not assume it.

Everything else still goes to `main` and ships in the next feature release.

### Create the release branch

`release/X.Y` is an ordinary public branch. A maintainer creates it when the line first needs a patch, starting from the latest published stable release on that line. For `release/6.3`, that is `v6.3.1` when `v6.3.1` is the newest 6.3 tag, and `v6.3.0` otherwise:

```sh
git fetch origin --tags
git switch -c release/6.3 v6.3.1
git push origin release/6.3
```

Before anything merges into `release/**`, create the `Release branches` ruleset on `refs/heads/release/**`. It must apply the same requirements as `PR Requirements` on `main`, and also block deletion and force-pushes.

### Bring the release pipeline up to date

A tag can predate changes to the release pipeline. When it does, the first pull request into the new branch is a narrowly reviewed infrastructure update. Copy over only what this release needs:

- the scripts, workflow files, and configuration the release runs
- changelog history, initialized only through the stable release the branch started from

Three things this pull request never does:

- copy `main`'s whole `.changes/unreleased/` directory
- replace all of `.github/` wholesale
- merge `main` into the release branch

Every file it touches has to be reviewable as part of this release. Open it before the fix, so the fix reviews on its own.

### Develop the fix on the release branch

Open the fix pull request against `release/X.Y` directly. It is reviewed and tested against the tree operators run, so what reviewers read is what ships.

If the same fix already exists on `main`, reuse it instead of rewriting it:

```sh
git switch release/6.3
git switch -c fix-peer-timeout-6.3
git cherry-pick -x -m 1 <merge commit of the main PR>
```

`-m 1` takes the merge commit's whole change as one commit. `-x` records the source commit in the message, so a reader can trace the branch commit back to its `main` pull request. This is a shortcut, not a requirement. When there is nothing to reuse, or the reused patch does not apply, write the fix on the branch and say what differs from `main` and why.

The pull request gets the same review and the same required checks as any other.

### Land the release metadata

Two pieces of release metadata are reviewed explicitly on the branch.

Versions and changelog entries come from the Release PR, generated the same way they are on `main`. Commit a change fragment under `.changes/unreleased/` with the fix, and let the Release workflow batch it.

`ESTIMATED_RELEASE_HEIGHT` in `zebrad/src/components/sync/end_of_support.rs` sets the support window for the version this patch publishes. Land a commit on the branch that sets it to the height the patch is expected to publish at. Renewing that height keeps the node running. It does not make the node follow the right chain, so it is not a substitute for the network-upgrade parameters a feature release carries.

### Release the patch

Publication works exactly as it does on `main`. release-plz opens `chore: release vX.Y.Z` against the release branch, a maintainer reviews and merges it, and the rest is automated. Review it with the [Release PR checklist](#review-the-release-pr) and read [What Happens After Merge](#what-happens-after-merge).

Release PR head branches are namespaced by base branch, so `main` gets `release-plz-main-*` and `release/6.3` gets `release-plz-6.3-*`. The workflow sets this from the branch it runs on. Without the namespacing, one branch's Release PR would suppress the other's, because release-plz deduplicates open Release PRs by head-branch prefix across every base branch.

Recovery uses the same dispatch as `main`, pointed at the branch:

```sh
gh workflow run release.yml --ref release/X.Y \
  -f operation=check \
  -f release_pr_number=<PR>
```

```sh
gh workflow run release.yml --ref release/X.Y \
  -f operation=resume \
  -f release_pr_number=<PR>
```

### What a patch publishes when it is not Latest

`ZcashFoundation/cargo-release` decides whether a release is marked Latest on GitHub. The binaries and deploy workflows always publish the immutable `X.Y.Z` artifacts, then recheck GitHub's latest-release marker immediately before mutating Docker Hub `latest` or deploying production.

A patch that is not marked Latest still publishes crates, tags, the GitHub Release, signed binaries, and the Docker Hub `X.Y.Z` tag. Operators who pin `zfnd/zebra:6.3.1` get the patch.

### Carry the fix into `main`

Every fix on a release branch must also reach `main`, through a forward-port pull request that someone tracks. A fix that stays on the branch disappears at the next feature release.

Open it as a normal pull request into `main`. When `main` has changed around the fix, adapt it there and let review cover the adaptation.

The next feature release from `main` has to include every applicable released fix and the correct network-upgrade parameters.

### Retire the release branch

Retiring a branch takes an explicit support decision. Do not infer it from the version `main` proposes, and do not infer it from a renewed end-of-support height.

Once maintainers decide the line is no longer supported, delete the branch. The tags, releases, crates, and images it published stay where they are.

---
name: 'Release Checklist Template'
about: 'Checklist to create and publish a Zebra release'
title: 'Release Zebra (version)'
labels: 'A-release, C-exclude-from-changelog, P-Critical :ambulance:'
assignees: ''

---

# Prepare for the Release

- [ ] Make sure there has been [at least one successful full sync test in the main branch](https://github.com/ZcashFoundation/zebra/actions/workflows/ci-tests.yml?query=branch%3Amain) since the last state change, or start a manual full sync.

# Checkpoints

For performance and security, we want to update the Zebra checkpoints in every release.
- [ ] You can copy the latest checkpoints from CI by following [the zebra-checkpoints README](https://github.com/ZcashFoundation/zebra/blob/main/zebra-utils/README.md#zebra-checkpoints).

# Missed Dependency Updates

Sometimes `dependabot` misses some dependency updates, or we accidentally turned them off.

This step can be skipped if there is a large pending dependency upgrade. (For example, shared ECC crates.)

Here's how we make sure we got everything:
- [ ] Run `cargo update` on the latest `main` branch, and keep the output
- [ ] If needed, [add duplicate dependency exceptions to deny.toml](https://github.com/ZcashFoundation/zebra/blob/main/book/src/dev/continuous-integration.md#fixing-duplicate-dependencies-in-check-denytoml-bans)
- [ ] If needed, remove resolved duplicate dependencies from `deny.toml`
- [ ] Open a separate PR with the changes
- [ ] Add the output of `cargo update` to that PR as a comment

# Summarise Release Changes

These steps can be done a few days before the release, in the same PR:

## Change Log

**Important**: Any merge into `main` deletes any edits to the draft changelog.
Once you are ready to tag a release, copy the draft changelog into `CHANGELOG.md`.

We use [the Release Drafter workflow](https://github.com/marketplace/actions/release-drafter) to automatically create a [draft changelog](https://github.com/ZcashFoundation/zebra/releases). We follow the [Keep a Changelog](https://keepachangelog.com/en/1.0.0/) format.

To create the final change log:
- [ ] Copy the [**latest** draft
  changelog](https://github.com/ZcashFoundation/zebra/releases) into
  `CHANGELOG.md` (there can be multiple draft releases)
- [ ] Delete any trivial changes
    - [ ] Put the list of deleted changelog entries in a PR comment to make reviewing easier
- [ ] Combine duplicate changes
- [ ] Edit change descriptions so they will make sense to Zebra users
- [ ] Check the category for each change
  - Prefer the "Fix" category if you're not sure

## README

README updates can be skipped for urgent releases.

Update the README to:
- [ ] Remove any "Known Issues" that have been fixed since the last release.
- [ ] Update the "Build and Run Instructions" with any new dependencies.
      Check for changes in the `Dockerfile` since the last tag: `git diff <previous-release-tag> docker/Dockerfile`.
- [ ] If Zebra has started using newer Rust language features or standard library APIs, update the known working Rust version in the README, book, and `Cargo.toml`s

You can use a command like:
```sh
fastmod --fixed-strings '1.58' '1.65'
```

## Create the Release PR

- [ ] Push the updated changelog and README into a new branch
      for example: `bump-v1.0.0` - this needs to be different to the tag name
- [ ] Create a release PR by adding `&template=release-checklist.md` to the comparing url ([Example](https://github.com/ZcashFoundation/zebra/compare/bump-v1.0.0?expand=1&template=release-checklist.md)).
- [ ] Freeze the [`batched` queue](https://dashboard.mergify.com/github/ZcashFoundation/repo/zebra/queues) using Mergify.
- [ ] Mark all the release PRs as `Critical` priority, so they go in the `urgent` Mergify queue.
- [ ] Mark all non-release PRs with `do-not-merge`, because Mergify checks approved PRs against every commit, even when a queue is frozen.
- [ ] Add the `A-release` tag to the release pull request in order for the `check_no_git_refs_in_cargo_lock` to run.

## Zebra git sources dependencies

- [ ] Ensure the `check_no_git_refs_in_cargo_lock` check passes.

This check runs automatically on pull requests with the `A-release` label. It must pass for crates to be published to crates.io. If the check fails, you should either halt the release process or proceed with the understanding that the crates will not be published on crates.io.

# Update Versions and End of Support

## Update Zebra Version

### Choose a Release Level

Zebra follows [semantic versioning](https://semver.org). Semantic versions look like: MAJOR.MINOR.PATCH[-TAG.PRE-RELEASE]

Choose a release level for `zebrad`. Release levels are based on user-visible changes from the changelog:
- Mainnet Network Upgrades are `major` releases
- significant new features or behaviour changes; changes to RPCs, command-line, or configs; and deprecations or removals are `minor` releases
- otherwise, it is a `patch` release

### Update Crate Versions and Crate Change Logs

If you're publishing crates for the first time, [log in to crates.io](https://zebra.zfnd.org/dev/crate-owners.html#logging-in-to-cratesio),
and make sure you're a member of owners group.

Check that the release will work:

- [ ] Determine which crates require release. Run `git diff --stat <previous_tag>`
      and enumerate the crates that had changes.
- [ ] Determine which type of release to make. Run `semver-checks` to list API
      changes: `cargo semver-checks -p <crate> --default-features`. If there are
      breaking API changes, do a major release, or try to revert the API change
      if it was accidental. Otherwise do a minor or patch release depending on
      whether a new API was added. Note that `semver-checks` won't work
      if the previous realase was yanked; you will have to determine the
      type of release manually.
- [ ] Update the crate `CHANGELOG.md` listing the API changes or other
      relevant information for a crate consumer. It might make sense to copy
      entries from the `zebrad` changelog.
- [ ] Update crate versions:

```sh
cargo release version --verbose --execute --allow-branch '*' -p <crate> patch # [ major | minor ]
cargo release replace --verbose --execute --allow-branch '*' -p <crate>
```

- [ ] Update the crate `CHANGELOG.md`
- [ ] Commit and push the above version changes to the release branch.

## Update End of Support

The end of support height is calculated from the current blockchain height:
- [ ] Find where the Zcash blockchain tip is now by using a [Zcash Block Explorer](https://mainnet.zcashexplorer.app/) or other tool.
- [ ] Replace `ESTIMATED_RELEASE_HEIGHT` in [`end_of_support.rs`](https://github.com/ZcashFoundation/zebra/blob/main/zebrad/src/components/sync/end_of_support.rs) with the height you estimate the release will be tagged.

<details>

<summary>Optional: calculate the release tagging height</summary>

- Add `1152` blocks for each day until the release
- For example, if the release is in 3 days, add `1152 * 3` to the current Mainnet block height

</details>

## Update the Release PR

- [ ] Push the version increments and the release constants to the release branch.


# Publish the Zebra Release

## Create the GitHub Pre-Release

- [ ] Wait for all the release PRs to be merged
- [ ] Create a new release using the draft release as a base, by clicking the Edit icon in the [draft release](https://github.com/ZcashFoundation/zebra/releases)
- [ ] Set the tag name to the version tag,
      for example: `v1.0.0`
- [ ] Set the release to target the `main` branch
- [ ] Set the release title to `Zebra ` followed by the version tag,
      for example: `Zebra 1.0.0`
- [ ] Replace the prepopulated draft changelog in the release description with the final changelog you created;
      starting just _after_ the title `## [Zebra ...` of the current version being released,
      and ending just _before_ the title of the previous release.
- [ ] Mark the release as 'pre-release', until it has been built and tested
- [ ] Publish the pre-release to GitHub using "Publish Release"
- [ ] Delete all the [draft releases from the list of releases](https://github.com/ZcashFoundation/zebra/releases)

## Test the Pre-Release

- [ ] Wait until the Docker binaries have been built on `main`, and the quick tests have passed:
    - [ ] [ci-tests.yml](https://github.com/ZcashFoundation/zebra/actions/workflows/ci-tests.yml?query=branch%3Amain)
- [ ] Wait until the [pre-release deployment machines have successfully launched](https://github.com/ZcashFoundation/zebra/actions/workflows/cd-deploy-nodes-gcp.yml?query=event%3Arelease)

## Publish Release

- [ ] [Publish the release to GitHub](https://github.com/ZcashFoundation/zebra/releases) by disabling 'pre-release', then clicking "Set as the latest release"

## Publish Crates

- [ ] [Run `cargo login`](https://zebra.zfnd.org/dev/crate-owners.html#logging-in-to-cratesio)
- [ ] Run `cargo clean` in the zebra repo (optional)
- [ ] Publish the crates to crates.io: `cargo release publish --verbose --workspace --execute`
- [ ] Check that Zebra can be installed from `crates.io`:
      `cargo install --locked --force --version 1.minor.patch zebrad && ~/.cargo/bin/zebrad`
      and put the output in a comment on the PR.

## Publish Docker Images

- [ ] Wait for the [the Docker images to be published successfully](https://github.com/ZcashFoundation/zebra/actions/workflows/release-binaries.yml?query=event%3Arelease).
- [ ] Wait for the new tag in the [dockerhub zebra space](https://hub.docker.com/r/zfnd/zebra/tags)
- [ ] Un-freeze the [`batched` queue](https://dashboard.mergify.com/github/ZcashFoundation/repo/zebra/queues) using Mergify.
- [ ] Remove `do-not-merge` from the PRs you added it to

## Release Failures

If building or running fails after tagging:

<details>

<summary>Tag a new release, following these instructions...</summary>

1. Fix the bug that caused the failure
2. Start a new `patch` release
3. Skip the **Release Preparation**, and start at the **Release Changes** step
4. Update `CHANGELOG.md` with details about the fix
5. Follow the release checklist for the new Zebra version

</details>

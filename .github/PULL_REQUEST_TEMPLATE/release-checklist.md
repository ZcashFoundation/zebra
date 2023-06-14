---
name: Release Checklist Template
about: Checklist of versioning to create a taggable commit for Zebra
title: ''
labels:
assignees: ''

---

# Prepare for the Release

These release steps can be done a week before the release, in separate PRs.
They can be skipped for urgent releases.

## Checkpoints

For performance and security, we want to update the Zebra checkpoints in every release.
- [ ] You can copy the latest checkpoints from CI by following [the zebra-checkpoints README](https://github.com/ZcashFoundation/zebra/blob/main/zebra-utils/README.md#zebra-checkpoints).

## Missed Dependency Updates

Sometimes `dependabot` misses some dependency updates, or we accidentally turned them off.

Here's how we make sure we got everything:
- [ ] Run `cargo update` on the latest `main` branch, and keep the output
- [ ] If needed, update [deny.toml](https://github.com/ZcashFoundation/zebra/blob/main/book/src/dev/continuous-integration.md#fixing-duplicate-dependencies-in-check-denytoml-bans)
- [ ] Open a separate PR with the changes
- [ ] Add the output of `cargo update` to that PR as a comment


# Make Release Changes

These release steps can be done a few days before the release, in the same PR:
- [ ] Make sure the PRs with the new checkpoint hashes and missed dependencies are already merged

## Versioning

### How to Increment Versions

Zebra follows [semantic versioning](https://semver.org). Semantic versions look like: MAJOR.MINOR.PATCH[-TAG.PRE-RELEASE]

Choose a release level for `zebrad` based on the changes in the release that users will see:
- mainnet network upgrades are `major` releases
- new features, large changes, deprecations, and removals are `minor` releases
- otherwise, it is a `patch` release

Zebra's Rust API doesn't have any support or stability guarantees, so we keep all the `zebra-*` and `tower-*` crates on a beta `pre-release` version.

### Update Crate Versions

<details>

<summary>If you're publishing crates for the first time:</summary>

- [ ] Install `cargo-release`: `cargo install cargo-release`
- [ ] Make sure you are  an owner of the crate or [a member of the Zebra crates.io `owners` group on GitHub](https://github.com/orgs/ZcashFoundation/teams/owners)

</details>

- [ ] Update crate versions and do a release dry-run:
    - [ ] `cargo release version --execute --verbose --workspace --exclude zebrad beta`
    - [ ] `cargo release version --execute --verbose --package zebrad [ major | minor | patch ]`
    - [ ] `cargo release publish --verbose --workspace --dry-run`
- [ ] Commit the version changes to your release PR branch using `git`: `cargo release commit --verbose --workspace`

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

## Change Log

**Important**: Any merge into `main` deletes any edits to the draft changelog.
Once you are ready to tag a release, copy the draft changelog into `CHANGELOG.md`.

We use [the Release Drafter workflow](https://github.com/marketplace/actions/release-drafter) to automatically create a [draft changelog](https://github.com/ZcashFoundation/zebra/releases). We follow the [Keep a Changelog](https://keepachangelog.com/en/1.0.0/) format.

To create the final change log:
- [ ] Copy the **latest** draft changelog into `CHANGELOG.md` (there can be multiple draft releases)
- [ ] Delete any trivial changes
    - [ ] Put the list of deleted changelog entries in a PR comment to make reviewing easier
- [ ] Combine duplicate changes
- [ ] Edit change descriptions so they will make sense to Zebra users
- [ ] Check the category for each change
  - Prefer the "Fix" category if you're not sure

## Update End of Support

The end of support height is calculated from the current blockchain height:
- [ ] Find where the Zcash blockchain tip is now by using a [Zcash explorer](https://zcashblockexplorer.com/blocks) or other tool.
- [ ] Replace `ESTIMATED_RELEASE_HEIGHT` in [`end_of_support.rs`](https://github.com/ZcashFoundation/zebra/blob/main/zebrad/src/components/sync/end_of_support.rs) with the height you estimate the release will be tagged.

<details>

<summary>Optional: calculate the release tagging height</summary>

- Add `1152` blocks for each day until the release
- For example, if the release is in 3 days, add `1152 * 3` to the current Mainnet block height

</details>

### Create the Release PR

- [ ] Push the version increments, the updated changelog, and the release constants into a branch,
      for example: `bump-v1.0.0` - this needs to be different to the tag name
- [ ] Create a release PR by adding `&template=release-checklist.md` to the comparing url ([Example](https://github.com/ZcashFoundation/zebra/compare/bump-v1.0.0?expand=1&template=release-checklist.md)).
- [ ] Freeze the [`batched` queue](https://dashboard.mergify.com/github/ZcashFoundation/repo/zebra/queues) using Mergify.
- [ ] Mark all the release PRs as `Critical` priority, so they go in the `urgent` Mergify queue.


# Release Zebra

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

- [ ] Wait until the [Docker binaries have been built on `main`](https://github.com/ZcashFoundation/zebra/actions/workflows/continous-integration-docker.yml), and the quick tests have passed.
      (You can ignore the full sync and `lightwalletd` tests, because they take about a day to run.)
- [ ] Wait until the [pre-release deployment machines have successfully launched](https://github.com/ZcashFoundation/zebra/actions/workflows/continous-delivery.yml)

## Publish Release

- [ ] [Publish the release to GitHub](https://github.com/ZcashFoundation/zebra/releases) by disabling 'pre-release', then clicking "Set as the latest release"

## Publish Crates

- [ ] Run `cargo login`
- [ ] Publish the crates to crates.io: `cargo release publish --verbose --workspace --execute`
- [ ] Check that Zebra can be installed from `crates.io`:
      `cargo install --force --version 1.0.0 zebrad && ~/.cargo/bin/zebrad`

## Publish Docker Images
- [ ] Wait until [the Docker images have been published](https://github.com/ZcashFoundation/zebra/actions/workflows/release-binaries.yml)
- [ ] Test the Docker image using `docker run --tty --interactive zfnd/zebra:v1.0.0`,
      and put the output in a comment on the PR. 
      (You can use [gcloud cloud shell](https://console.cloud.google.com/home/dashboard?cloudshell=true))
- [ ] Un-freeze the [`batched` queue](https://dashboard.mergify.com/github/ZcashFoundation/repo/zebra/queues) using Mergify.

## Release Failures

If building or running fails after tagging:

<details>

1. Fix the bug that caused the failure
2. Start a new `patch` release
3. Skip the **Release Preparation**, and start at the **Release Changes** step
4. Update `CHANGELOG.md` with details about the fix
5. Follow the release checklist for the new Zebra version

</details>

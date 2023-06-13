---
name: Release Checklist Template
about: Checklist of versioning to create a taggable commit for Zebra
title: ''
labels:
assignees: ''

---

## Versioning

### How to Increment Versions

Zebra follows [semantic versioning](https://semver.org). Semantic versions look like: MAJOR`.`MINOR`.`PATCH[`-`TAG`.`PRE-RELEASE]

The [draft `zebrad` changelog](https://github.com/ZcashFoundation/zebra/releases) will have an automatic version bump. This version is based on [the labels on the PRs in the release](https://github.com/ZcashFoundation/zebra/blob/main/.github/release-drafter.yml).

Check that the automatic `zebrad` version increment matches the changes in the release:

<details>

If we're releasing a mainnet network upgrade, it is a `major` release:
1. Increment the `major` version of _*all*_ the Zebra crates.
2. Increment the `patch` version of the tower crates.

If we're not releasing a mainnet network upgrade, check for features, major changes, deprecations, and removals. If this release has any, it is a `minor` release:
1. Increment the `minor` version of `zebrad`.
2. Increment the `pre-release` version of the other crates.
3. Increment the `patch` version of the tower crates.

Otherwise, it is a `patch` release:
1. Increment the `patch` version of `zebrad`.
2. Increment the `pre-release` version of the other crates.
3. Increment the `patch` version of the tower crates.

Zebra's Rust API is not stable or supported, so we keep all the crates on the same beta `pre-release` version.

</details>

### Version Locations

Once you know which versions you want to increment, you can find them in the:

zebrad (rc):
- [ ] zebrad `Cargo.toml`
- [ ] `README.md`
- [ ] `book/src/user/docker.md`

crates (beta):
- [ ] zebra-* `Cargo.toml`s

tower (patch):
- [ ] tower-* `Cargo.toml`s

auto-generated:
- [ ] `Cargo.lock`: run `cargo build` after updating all the `Cargo.toml`s

#### Version Tooling

You can use `fastmod` to interactively find and replace versions.

For example, you can do something like:
```
fastmod --extensions rs,toml,md --fixed-strings '1.0.0' '1.1.0' zebrad README.md zebra-network/src/constants.rs book/src/user/docker.md
fastmod --extensions rs,toml,md --fixed-strings '1.0.0-beta.15' '1.0.0-beta.16' zebra-*
fastmod --extensions rs,toml,md --fixed-strings '0.2.30' '0.2.31' tower-batch tower-fallback
cargo build
```

If you use `fastmod`, don't update versions in `CHANGELOG.md` or `zebra-dependencies-for-audit.md`.

## README

Update the README to:
- [ ] Remove any "Known Issues" that have been fixed
- [ ] Update the "Build and Run Instructions" with any new dependencies.
      Check for changes in the `Dockerfile` since the last tag: `git diff <previous-release-tag> docker/Dockerfile`.
- [ ] If Zebra has started using newer Rust language features or standard library APIs, update the known working Rust version in the README, book, and `Cargo.toml`s

You can use a command like:
```sh
      fastmod --fixed-strings '1.58' '1.65'
```

## Checkpoints

For performance and security, we want to update the Zebra checkpoints in every release.
You can copy the latest checkpoints from CI by following [the zebra-checkpoints README](https://github.com/ZcashFoundation/zebra/blob/main/zebra-utils/README.md#zebra-checkpoints).

## Missed Dependency Updates

Sometimes `dependabot` misses some dependency updates, or we accidentally turned them off.

Here's how we make sure we got everything:
- [ ] Run `cargo update` on the latest `main` branch, and keep the output
- [ ] If needed, update [deny.toml](https://github.com/ZcashFoundation/zebra/blob/main/book/src/dev/continuous-integration.md#fixing-duplicate-dependencies-in-check-denytoml-bans)
- [ ] Open a separate PR with the changes
- [ ] Add the output of `cargo update` to that PR as a comment

## Change Log

**Important**: Any merge into `main` deletes any edits to the draft changelog.
Once you are ready to tag a release, copy the draft changelog into `CHANGELOG.md`.

We use [the Release Drafter workflow](https://github.com/marketplace/actions/release-drafter) to automatically create a [draft changelog](https://github.com/ZcashFoundation/zebra/releases). We follow the [Keep a Changelog](https://keepachangelog.com/en/1.0.0/) format.

To create the final change log:
- [ ] Copy the **latest** draft changelog into `CHANGELOG.md` (there can be multiple draft releases)
- [ ] Delete any trivial changes. Keep the list of those, to include in the PR
- [ ] Combine duplicate changes
- [ ] Edit change descriptions so they are consistent, and make sense to non-developers
- [ ] Check the category for each change
  - Prefer the "Fix" category if you're not sure

<details>

#### Change Categories

From "Keep a Changelog":
* `Added` for new features.
* `Changed` for changes in existing functionality.
* `Deprecated` for soon-to-be removed features.
* `Removed` for now removed features.
* `Fixed` for any bug fixes.
* `Security` in case of vulnerabilities.

</details>

## Release support constants

Needed for the end of support feature. Please update the following constants [in this file](https://github.com/ZcashFoundation/zebra/blob/main/zebrad/src/components/sync/end_of_support.rs):

- [ ] `ESTIMATED_RELEASE_HEIGHT` (required) - Replace with the estimated height you estimate the release will be tagged.
      <details>
      - Find where the Zcash blockchain tip is now by using a [Zcash explorer](https://zcashblockexplorer.com/blocks) or other tool.
      -  Consider there are aprox `1152` blocks per day (with the current Zcash `75` seconds spacing).
      - So for example if you think the release will be tagged somewhere in the next 3 days you can add `1152 * 3` to the current tip height and use that value here.
      </details>

## Create the Release

### Create the Release PR

After you have the version increments, the updated checkpoints, any missed dependency updates,
and the updated changelog:

- [ ] Make sure the PRs with the new checkpoint hashes and missed dependencies are already merged
- [ ] Push the version increments, the updated changelog and the release constants into a branch
      (for example: `bump-v1.0.0` - this needs to be different to the tag name)
- [ ] Create a release PR by adding `&template=release-checklist.md` to the comparing url ([Example](https://github.com/ZcashFoundation/zebra/compare/bump-v1.0.0?expand=1&template=release-checklist.md)).
  - [ ] Add the list of deleted changelog entries as a comment to make reviewing easier.
- [ ] Freeze the [`batched` queue](https://dashboard.mergify.com/github/ZcashFoundation/repo/zebra/queues) using Mergify.
- [ ] Mark all the release PRs as `Critical` priority, so they go in the `urgent` Mergify queue.

### Create the Release

- [ ] Once the PR has been merged, create a new release using the draft release as a base, by clicking the Edit icon in the [draft release](https://github.com/ZcashFoundation/zebra/releases)
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

## Publish crates

- [ ] Run `cargo login`
- [ ] Publish the crates to crates.io: `cargo release publish --verbose --workspace --execute`

## Binary Testing

- [ ] Wait until the [Docker binaries have been built on `main`](https://github.com/ZcashFoundation/zebra/actions/workflows/continous-integration-docker.yml), and the quick tests have passed.
      (You can ignore the full sync and `lightwalletd` tests, because they take about a day to run.)
- [ ] Wait until the [pre-release deployment machines have successfully launched](https://github.com/ZcashFoundation/zebra/actions/workflows/continous-delivery.yml)
- [ ] [Publish the release to GitHub](https://github.com/ZcashFoundation/zebra/releases) by disabling 'pre-release', then clicking "Set as the latest release"
- [ ] Wait until [the Docker images have been published](https://github.com/ZcashFoundation/zebra/actions/workflows/release-binaries.yml)
- [ ] Test the Docker image using `docker run --tty --interactive zfnd/zebra:v1.0.0`,
      and put the output in a comment on the PR. 
      (You can use [gcloud cloud shell](https://console.cloud.google.com/home/dashboard?cloudshell=true))
- [ ] Un-freeze the [`batched` queue](https://dashboard.mergify.com/github/ZcashFoundation/repo/zebra/queues) using Mergify.


## Telling Zebra Users

- [ ] Post a summary of the important changes in the release in the `#arborist` and `#communications` Slack channels

## Release Failures

If building or running fails after tagging:

<details>

1. Fix the bug that caused the failure
2. Increment the patch version again, following these instructions from the start
3. Update the code and documentation with a **new** git tag
4. Update `CHANGELOG.md` with details about the fix
5. Tag a **new** release

</details>

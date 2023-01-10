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

Check that the automatic `zebrad` version increment is correct:

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

### Version Locations

Once you know which versions you want to increment, you can find them in the:

zebrad (rc):
- [ ] zebrad `Cargo.toml`
- [ ] `zebra-network` protocol user agent: https://github.com/ZcashFoundation/zebra/blob/main/zebra-network/src/constants.rs
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
fastmod --extensions rs,toml,md --fixed-strings '1.0.0-rc.0' '1.0.0-rc.1' zebrad README.md zebra-network/src/constants.rs book/src/user/docker.md
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

With every release and for performance reasons, we want to update the Zebra checkpoints. More information on how to do this can be found in [the zebra-checkpoints README](https://github.com/ZcashFoundation/zebra/blob/main/zebra-consensus/src/checkpoint/README.md).

To do this you will need a synchronized `zcashd` node. You can request help from other zebra team members to submit this PR if you can't make it yourself at the moment of the release.

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

## Create the Release

### Create the Release PR

After you have the version increments, the updated checkpoints and the updated changelog:

- [ ] Make sure the PR with the new checkpoint hashes is already merged, or make it part of the changelog PR
- [ ] Push the version increments and the updated changelog into a branch
      (for example: `bump-v1.0.0-rc.0` - this needs to be different to the tag name)
- [ ] Create a release PR by adding `&template=release-checklist.md` to the comparing url ([Example](https://github.com/ZcashFoundation/zebra/compare/v1.0.0-rc.0-release?expand=1&template=release-checklist.md)).
  - [ ] Add the list of deleted changelog entries as a comment to make reviewing easier.
- [ ] Turn on [Merge Freeze](https://www.mergefreeze.com/installations/3676/branches).
- [ ] Once the PR is ready to be merged, unfreeze it [here](https://www.mergefreeze.com/installations/3676/branches).
      Do not unfreeze the whole repository.
- [ ] Update the PR to the latest `main` branch using `@mergifyio update`. Then Mergify should merge it in-place.
      If it makes a merge PR instead, that PR will get cancelled by the merge freeze. So just merge the changelog PR manually.

### Create the Release

- [ ] Once the PR has been merged, create a new release using the draft release as a base, by clicking the Edit icon in the [draft release](https://github.com/ZcashFoundation/zebra/releases)
- [ ] Set the tag name to the version tag,
      for example: `v1.0.0-rc.0`
- [ ] Set the release to target the `main` branch
- [ ] Set the release title to `Zebra ` followed by the version tag,
      for example: `Zebra 1.0.0-rc.0`
- [ ] Replace the prepopulated draft changelog in the release description with the final changelog you created;
      starting just _after_ the title `## [Zebra ...` of the current version being released,
      and ending just _before_ the title of the previous release.
- [ ] Mark the release as 'pre-release', until it has been built and tested
- [ ] Publish the pre-release to GitHub using "Publish Release"
- [ ] Delete all the [draft releases from the list of releases](https://github.com/ZcashFoundation/zebra/releases)

## Binary Testing

- [ ] Wait until the [Docker binaries have been built on `main`](https://github.com/ZcashFoundation/zebra/actions/workflows/continous-integration-docker.yml), and the quick tests have passed.
      (You can ignore the full sync and `lightwalletd` tests, because they take about a day to run.)
- [ ] [Publish the release to GitHub](https://github.com/ZcashFoundation/zebra/releases) by disabling 'pre-release', then clicking "Set as the latest release"
- [ ] Wait until [the Docker images have been published](https://github.com/ZcashFoundation/zebra/actions/workflows/release-binaries.yml)
- [ ] Test the Docker image using `docker run --tty --interactive zfnd/zebra:1.0.0-rc.<version>` <!-- TODO: replace with `zfnd/zebra` when we release 1.0.0 -->
- [ ] Turn off [Merge Freeze](https://www.mergefreeze.com/installations/3676/branches) for the whole repository


## Blog Post

If the release contains new features (`major` or `minor`), or high-priority bug fixes:
- [ ] Ask the team about doing a blog post

## Release Failures

If building or running fails after tagging:
1. Fix the bug that caused the failure
2. Increment versions again, following these instructions from the start
3. Update the code and documentation with a **new** git tag
4. Update `CHANGELOG.md` with details about the fix
5. Tag a **new** release

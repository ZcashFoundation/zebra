---
name: Release Checklist Template
about: Checklist of versioning to create a taggable commit for Zebra
title: ''
labels:
assignees: ''

---

## Versioning

### Which Crates to Increment

To check if any of the top-level crates need version increments:
1. Go to the zebra GitHub code page: https://github.com/ZcashFoundation/zebra
2. Check if the last commit was a Zebra version bump, or something else

<details>
   
Alternatively you can:
- Use the github compare tool and check the `main` branch against the last tag ([Example](https://github.com/ZcashFoundation/zebra/compare/v1.0.0-alpha.15...main))
- Use `git diff --stat <previous-release-tag> origin/main`

</details>
   
Once you know which crates have changed:
- [ ] Increment the crates that have new commits since the last version update
- [ ] Increment any crates that depend on crates that have changed
- [ ] Keep a list of the crates that haven't been incremented, to include in the PR

### How to Increment Versions

Zebra follows [semantic versioning](https://semver.org).

Semantic versions look like: MAJOR`.`MINOR`.`PATCH[`-`TAG`.`PRE-RELEASE]

### Reviewing Version Bumps

Check for missed changes by going to:
`https://github.com/ZcashFoundation/zebra/tree/<commit-hash>/`
Where `<commit-hash>` is the hash of the last commit in the version bump PR.

If any Zebra or Tower crates have commit messages that are **not** a version bump, we have missed an update.
Also check for crates that depend on crates that have changed. They should get a version bump as well.
   
### Version Locations

Once you know which versions you want to increment, you can find them in the:
- [ ] zebra* `Cargo.toml`s
- [ ] tower-* `Cargo.toml`s
- [ ] `zebra-network` protocol user agent: https://github.com/ZcashFoundation/zebra/blob/main/zebra-network/src/constants.rs
- [ ] `README.md`
- [ ] `book/src/user/install.md`
- [ ] `Cargo.lock`: run `cargo build` after updating all the `Cargo.toml`s

#### Version Tooling

You can use `fastmod` to interactively find and replace versions.

For example, you can do something like:
```
fastmod --extensions rs,toml,md --fixed-strings '1.0.0-beta.11' '1.0.0-beta.12'
fastmod --extensions rs,toml,md --fixed-strings '0.2.26' '0.2.27' tower-batch tower-fallback
```

If you use `fastmod`, don't update versions in `CHANGELOG.md`.

## README

We should update the README to:
- [ ] Remove any "Known Issues" that have been fixed
- [ ] Update the "Build and Run Instructions" with any new dependencies.
      Check for changes in the `Dockerfile` since the last tag: `git diff <previous-release-tag> docker/Dockerfile`.

## Checkpoints

With every release and for performance reasons, we want to update the zebra checkpoints. More information on how to do this can be found in [the zebra-checkpoints README](https://github.com/ZcashFoundation/zebra/blob/main/zebra-consensus/src/checkpoint/README.md).

To do this you will need a synchronized `zcashd` node. You can request help from other zebra team members to submit this PR if you can't make it yourself at the moment of the release.

## Change Log

**Important**: Any merge into `main` deletes any edits to the draft changelog.
Once you are ready to tag a release, copy the draft changelog into `CHANGELOG.md`.

We follow the [Keep a Changelog](https://keepachangelog.com/en/1.0.0/) format.

We use [the Release Drafter workflow](https://github.com/marketplace/actions/release-drafter) to automatically create a [draft changelog](https://github.com/ZcashFoundation/zebra/releases).

To create the final change log:
- [ ] Copy the draft changelog into `CHANGELOG.md`
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

- [ ] Make sure the PR with the new checkpoint hashes is already merged.
- [ ] Push the version increments and the updated changelog into a branch
      (name suggestion, example: `v100-alpha0-release`)
- [ ] Create a release PR by adding `&template=release-checklist.md` to the
      comparing url ([Example](https://github.com/ZcashFoundation/zebra/compare/v100-alpha0-release?expand=1&template=release-checklist.md)).
  - [ ] Add the list of deleted changelog entries as a comment to make reviewing easier.
  - [ ] Also add the list of not-bumped crates as a comment (can use the same comment as the previous one).
- [ ] Turn on [Merge Freeze](https://www.mergefreeze.com/installations/3676/branches).
- [ ] Once the PR is ready to be merged, unfreeze it [here](https://www.mergefreeze.com/installations/3676/branches).
      Do not unfreeze the whole repository.

### Create the Release

- [ ] Once the PR has been merged,
      create a new release using the draft release as a base,
      by clicking the Edit icon in the [draft release](https://github.com/ZcashFoundation/zebra/releases)
- [ ] Set the tag name to the version tag,
      for example: `v1.0.0-alpha.0`
- [ ] Set the release to target the `main` branch
- [ ] Set the release title to `Zebra ` followed by the version tag,
      for example: `Zebra 1.0.0-alpha.0`
- [ ] Replace the prepopulated draft changelog in the release description by the final
      changelog you created; starting just _after_ the title `## [Zebra ...` of
      the current version being released, and ending just _before_ the title of
      the previous release.
- [ ] Mark the release as 'pre-release', until it has been built and tested
- [ ] Publish the pre-release to GitHub using "Publish Release"

## Build and Binary Testing

- [ ] After tagging the release, test that the exact `cargo install` command in `README.md` works
      (`--git` behaves a bit differently to `--path`)
- [ ] Wait until the Docker binaries have been built, and the quick tests have passed.
      (You can ignore the full sync and `lightwalletd` tests, because they take about a day to run.)
- [ ] Publish the release to GitHub by disabling 'pre-release', then clicking "Publish Release"
- [ ] Turn off [Merge Freeze](https://www.mergefreeze.com/installations/3676/branches) for the whole repository

If the build fails after tagging:
1. Fix the build
2. Increment versions again, following these instructions from the start
3. Update `README.md` with a **new** git tag
4. Update `CHANGELOG.md` with details about the fix
5. Tag a **new** release

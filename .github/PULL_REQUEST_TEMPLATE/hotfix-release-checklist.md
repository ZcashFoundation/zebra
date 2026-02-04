---
name: 'Hotfix Release Checklist Template'
about: 'Checklist to create and publish a hotfix Zebra release'
title: 'Release Zebra (version)'
labels: 'A-release, C-exclude-from-changelog, P-Critical :ambulance:'
assignees: ''

---

A hotfix release should only be created when a bug or critical issue is discovered in an existing release, and waiting for the next scheduled release is impractical or unacceptable.

## Create the Release PR

- [ ] Create a branch to fix the issue based on the tag of the release being fixed (not the main branch).
      for example: `hotfix-v2.3.1` - this needs to be different to the tag name
- [ ] Make the required changes
- [ ] Create a hotfix release PR by adding `&template=hotfix-release-checklist.md` to the comparing url ([Example](https://github.com/ZcashFoundation/zebra/compare/bump-v1.0.0?expand=1&template=hotfix-release-checklist.md)).
- [ ] Add the `C-exclude-from-changelog` label so that the PR is omitted from the next release changelog
- [ ] Add the `A-release` tag to the release pull request in order for the `check_no_git_refs_in_cargo_lock` to run.
- [ ] Add the `do-not-merge` tag to prevent Mergify from merging, since after PR approval the
      release is done from the branch itself.
- [ ] Ensure the `check_no_git_refs_in_cargo_lock` check passes.
- [ ] Add a changelog entry for the release summarizing user-visible changes.

## Update Versions

If it is a Zebra hotfix, the release level for should always follow semantic
versioning as a `patch` release. If it is a crate hotfix, it should simply
follow semver, depending on the thing being fixed.

- [ ] Follow the "Update Zebra Version" section in the regular checklist for
  instructions

## Update the Release PR

- [ ] Push the version increments and the release constants to the hotfix release branch.

# Publish the Release

## Create the GitHub Pre-Release (if Zebra hotfix)

- [ ] Wait for the hotfix release PR to be reviewed, approved, and merged into main.
- [ ] Create a new release
- [ ] Set the tag name to the version tag,
      for example: `v2.3.1`
- [ ] Set the release to target the hotfix release branch
- [ ] Set the release title to `Zebra ` followed by the version tag,
      for example: `Zebra 2.3.1`
- [ ] Populate the release description with the final changelog you created;
      starting just _after_ the title `## [Zebra ...` of the current version being released,
      and ending just _before_ the title of the previous release.
- [ ] Mark the release as 'pre-release', until it has been built and tested
- [ ] Publish the pre-release to GitHub using "Publish Release"

## Test the Pre-Release (if Zebra hotfix)

- [ ] Wait until the Docker binaries have been built on the hotfix release branch, and the quick tests have passed:
    - [ ] [ci-tests.yml](https://github.com/ZcashFoundation/zebra/actions/workflows/ci-tests.yml)
- [ ] Wait until the [pre-release deployment machines have successfully launched](https://github.com/ZcashFoundation/zebra/actions/workflows/zfnd-deploy-nodes-gcp.yml?query=event%3Arelease)

## Publish Release (if Zebra hotfix)

- [ ] [Publish the release to GitHub](https://github.com/ZcashFoundation/zebra/releases) by disabling 'pre-release', then clicking "Set as the latest release"

## Publish Crates

- [ ] Checkout the hotfix release branch
- [ ] [Run `cargo login`](https://zebra.zfnd.org/dev/crate-owners.html#logging-in-to-cratesio)
- [ ] Run `cargo clean` in the zebra repo
- [ ] Publish the crates to crates.io: `cargo release publish --verbose --workspace --execute --allow-branch {hotfix-release-branch}`
- [ ] Check that the published version of Zebra can be installed from `crates.io`:
      `cargo install --locked --force --version 2.minor.patch zebrad && ~/.cargo/bin/zebrad`
      and put the output in a comment on the PR.

## Publish Docker Images (if Zebra hotfix)

- [ ] Wait for the [the Docker images to be published successfully](https://github.com/ZcashFoundation/zebra/actions/workflows/release-binaries.yml?query=event%3Arelease).
- [ ] Wait for the new tag in the [dockerhub zebra space](https://hub.docker.com/r/zfnd/zebra/tags)

## Merge hotfix into main

- [ ] Solve any conflicts between the hotfix branch and main. Do not force-push
      into the branch! We need to include the commit that was released into `main`.
- [ ] Get the PR reviewed again if changes were made
- [ ] Admin-merge the PR with a merge commit (if by the time you are following
      this we have switched to merge commits by default, then just remove
      the `do-not-merge` label)

## Release Failures

If building or running fails after tagging:

<details>
1. Create a new hotfix release, starting from the top of this document.
</details>

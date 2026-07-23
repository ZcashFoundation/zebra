---
name: "🚀 Zebra Release"
about: "Zebra team use only"
title: "Publish next Zebra release: (version)"
labels: "A-release, C-exclude-from-changelog, P-Medium :zap:"
assignees: ""
---

# Prepare for the Release

These release steps can be done a week before the release, in separate PRs.
They can be skipped for urgent releases.

## State Full Sync Test

To check consensus correctness, we want to test that the state format is valid after a full sync. (Format upgrades are tested in CI on each PR.)

- [ ] Make sure there has been [at least one successful full sync test](https://github.com/ZcashFoundation/zebra/actions/workflows/zfnd-ci-integration-tests-gcp.yml?query=event%3Aschedule) since the last state change, or
- [ ] Start a manual workflow run of [`zfnd-ci-integration-tests-gcp.yml`](https://github.com/ZcashFoundation/zebra/actions/workflows/zfnd-ci-integration-tests-gcp.yml) with both `run-full-sync: true` and `run-lwd-sync: true`.

State format changes can be made in `zebra-state` or `zebra-chain`. The state format can be changed by data that is sent to the state, data created within the state using `zebra-chain`, or serialization formats in `zebra-state` or `zebra-chain`.

After the test has been started, or if it has finished already:

- [ ] Ask for a state code freeze in Slack. The freeze lasts until the release has been published.

## Checkpoints

For performance and security, we want to update the Zebra checkpoints in every release.

- [ ] You can copy the latest checkpoints from CI by following [the zebra-checkpoints README](https://github.com/ZcashFoundation/zebra/blob/main/zebra-utils/README.md#zebra-checkpoints).

## Missed Dependency Updates

Sometimes `dependabot` misses some dependency updates, or we accidentally turned them off.

This step can be skipped if there is a large pending dependency upgrade. (For example, shared ECC crates.)

Here's how we make sure we got everything:

- [ ] Run `cargo update` on the latest `main` branch, and keep the output
- [ ] If needed, [add duplicate dependency exceptions to deny.toml](https://github.com/ZcashFoundation/zebra/blob/main/book/src/dev/continuous-integration.md#fixing-duplicate-dependencies-in-check-denytoml-bans)
- [ ] If needed, remove resolved duplicate dependencies from `deny.toml`
- [ ] Open a separate PR with the changes
- [ ] Add the output of `cargo update` to that PR as a comment

# Prepare and Publish the Release

release-plz creates and updates the Release PR. After it is merged,
`ZcashFoundation/cargo-release` reconciles the planned release from its
immutable merge commit. Tag and GitHub Release policy lives only in
`.github/cargo-release.yml`.

## Check Before Merge

Run this read-only check against the open Release PR:

```sh
gh workflow run release.yml --ref main \
  -f operation=check \
  -f release_pr_number=<PR>
```

The check runs Cargo's complete multi-package dry-run and observes crates.io,
tags, and GitHub Releases. It never publishes a crate, creates a tag, or edits a
GitHub Release. Before the first publication, a green result with
`reason: "incomplete"` is expected: it confirms that the plan is valid while
the planned external state is still missing.

- [ ] Review and approve the complete generated Release PR checklist.
- [ ] Run the read-only check and review its plan and report.
- [ ] Merge the exact approved Release PR commit.

## Publish and Recover

After merge, the Release workflow publishes missing crates, verifies that the
published `zebrad` installs when applicable, then finalizes tags and the public
GitHub Release. The GitHub Release starts the binary, Docker, and GCP release
workflows.

If this post-merge workflow is interrupted, resume the same Release PR:

```sh
gh workflow run release.yml --ref main \
  -f operation=resume \
  -f release_pr_number=<PR>
```

`resume` is available only for releases whose merge commit contains
`.github/cargo-release.yml`. It can repair configured mutable GitHub Release
metadata, but it stops on immutable contradictions such as mismatched crate
provenance, a tag that targets another commit, or a release-channel conflict.
Do not publish crates, replace tags, or create the GitHub Release manually;
escalate those conflicts for maintainer review.

For a maintainer-authorized manual fallback, use the
[legacy release checklist](../PULL_REQUEST_TEMPLATE/release-checklist-legacy.md).

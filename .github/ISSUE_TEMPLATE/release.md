---
name: "🚀 Zebra Release"
about: 'Zebra team use only'
title: 'Publish next Zebra release: (version)'
labels: 'A-release, C-exclude-from-changelog, P-Medium :zap:'
assignees: ''

---

# Prepare for the Release

These release steps can be done a week before the release, in separate PRs.
They can be skipped for urgent releases.

## State Full Sync Test

To check consensus correctness, we want to test that the state format is valid after a full sync. (Format upgrades are tested in CI on each PR.)

- [ ] Make sure there has been [at least one successful full sync test](https://github.com/ZcashFoundation/zebra/actions/workflows/ci-tests.yml?query=event%3Aschedule) since the last state change, or
- [ ] Start a manual workflow run with a Zebra and `lightwalletd` full sync.

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

Follow the steps in the [release checklist](https://github.com/ZcashFoundation/zebra/blob/main/.github/PULL_REQUEST_TEMPLATE/release-checklist.md) to prepare the release:

Release PR:
- [ ] Update Changelog
- [ ] Update README
- [ ] Update Zebra Versions
- [ ] Update End of Support Height

Publish Release:
- [ ] Create & Test GitHub Pre-Release
- [ ] Publish GitHub Release
- [ ] Publish Rust Crates
- [ ] Publish Docker Images

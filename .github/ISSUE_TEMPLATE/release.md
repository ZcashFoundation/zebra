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

The automated release workflow creates the Release PR and embeds its checklist from `.release-plz.toml`. Review and merge that generated PR to start publication from its immutable merge commit.

- [ ] Review and approve the complete generated Release PR checklist.
- [ ] Merge the exact approved Release PR commit.
- [ ] Confirm the Release workflow publishes every planned crate, reconciles every tag, verifies `zebrad` installation when applicable, and creates the public GitHub Release last.

If the workflow is interrupted, rerun the **Release** workflow with the merged Release PR number. Do not publish crates, replace tags, or create the GitHub Release manually. Escalate any provenance mismatch, wrong-target tag, release-channel conflict, invalid Release PR, or exhausted crates.io retry instead of overwriting external state.

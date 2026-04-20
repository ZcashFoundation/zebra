---
name: "Release Checklist"
about: "Checklist for reviewing and merging a release-plz Release PR"
title: "Release Zebra (version)"
labels: "A-release, P-Critical"
assignees: ""
---

## Review the Release PR

- [ ] Version bumps match user-visible changes for each changed crate (`major` for network upgrades or breaking API, `minor` for new API/behavior/RPC/config, `patch` otherwise)
- [ ] `cargo-semver-checks` output has been reviewed for each published library crate
- [ ] Public API diff has been reviewed for each published library crate (`cargo public-api diff latest -p <crate> -sss`)
- [ ] Root `CHANGELOG.md` summary is clear and user-facing
- [ ] Per-crate `CHANGELOG.md` entries describe crate-consumer impact
- [ ] No git dependencies remain in crates that will publish to crates.io (`check-no-git-dependencies`)
- [ ] Checkpoint data is up to date (if applicable)
- [ ] End-of-support height is correct (if applicable)

## Verify CI and Release Inputs

- [ ] All CI checks pass on the Release PR
- [ ] A full-sync test has passed on `main` since the last state change, or a manual full sync is running
- [ ] Trusted publishing is configured for every crate release-plz will publish
- [ ] Release-plz release body only contains current release facts; update `[FILL IN]` sections before publishing the draft GitHub Release

## Merge and Monitor

- [ ] Merge the Release PR
- [ ] Verify crates published to crates.io (check release workflow output)
- [ ] Verify git tags created
- [ ] Verify draft GitHub Release created
- [ ] Fill in the `[FILL IN]` sections and click **Publish release**
- [ ] Verify `zebrad` installs from crates.io: `cargo install --locked --force --version <version> zebrad && ~/.cargo/bin/zebrad`
- [ ] Verify Docker images built (check release-binaries workflow)
- [ ] Verify Docker Hub tag appears
- [ ] Verify GCP deployment triggered (check deploy-nodes workflow)

## If Something Goes Wrong

Re-run the failed job first. release-plz is idempotent: it recreates the Release PR, skips crates whose tags exist, and skips crates already on crates.io. The scenarios below need manual steps.

### A crate is on crates.io but no tag was created

The one case release-plz will not self-heal: on re-run it sees the crate as "done" and skips without creating the tag. Detect by comparing `cargo info <crate>` against `git ls-remote --tags origin`. Recover with:

```bash
gh release create <crate>-v<X.Y.Z> --target <release-commit-sha> --generate-notes
```

For `zebrad` the tag is `v<X.Y.Z>`. Promote from draft once verified.

### Roll back a bad release

1. `cargo yank <crate> --vers <X.Y.Z>` for each crate that should not be consumed.
2. Delete the GitHub Release; keep the tag because crates.io versions cannot be reused.
3. Bump to `<X.Y.Z+1>` with the fix in the next Release PR.

### Block future Release PRs during an incident

Add the `do-not-merge` label on the open Release PR.

### Emergency fallback to the old manual process

See [`release-checklist-legacy.md`](./release-checklist-legacy.md).

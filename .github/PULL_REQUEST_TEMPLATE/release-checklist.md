---
name: "Release Checklist"
about: "Checklist for reviewing and merging a release-plz Release PR"
title: "Release Zebra (version)"
labels: "A-release, P-Critical :ambulance:"
assignees: ""
---

## Review the Release PR

- [ ] Version bumps match user-visible changes for each changed crate (`major` for network upgrades or breaking API, `minor` for new API/behavior/RPC/config, `patch` otherwise)
- [ ] `cargo-semver-checks` output has been reviewed for each published library crate
- [ ] Public API diff has been reviewed for each published library crate (`cargo public-api diff latest -p <crate> -sss`)
- [ ] Root `CHANGELOG.md` summary is clear and user-facing
- [ ] Per-crate `CHANGELOG.md` entries describe crate-consumer impact
- [ ] No git dependencies remain in crates that will publish to crates.io (`check-no-git-dependencies`)
- [ ] Any required checkpoint, end-of-support height, README, or operational release-note changes already landed before this generated Release PR

## Verify CI and Release Inputs

- [ ] All CI checks pass on the Release PR
- [ ] A full-sync test has passed on `main` since the last state change, or a manual full sync is running
- [ ] Trusted publishing is configured for every crate release-plz will publish
- [ ] `RELEASE_APP_ID` and `RELEASE_APP_PRIVATE_KEY` are configured for the release environment, so the generated GitHub Release can trigger Docker and GCP workflows

## Merge and Monitor

- [ ] Merge the Release PR
- [ ] Verify crates published to crates.io (check the Release workflow output)
- [ ] Verify git tags were created
- [ ] Verify the public `zebrad` GitHub Release was created automatically
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

For `zebrad` the tag is `v<X.Y.Z>`. Create the GitHub Release with the release app token so downstream release workflows run.

### Roll back a bad release

1. `cargo yank <crate> --vers <X.Y.Z>` for each crate that should not be consumed.
2. Delete the GitHub Release; keep the tag because crates.io versions cannot be reused.
3. Bump to `<X.Y.Z+1>` with the fix in the next Release PR.

### Block future Release PRs during an incident

Add the `do-not-merge` label on the open Release PR.

### Emergency fallback to the old manual process

See [`release-checklist-legacy.md`](./release-checklist-legacy.md).

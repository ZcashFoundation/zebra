---
name: "Release Checklist"
about: "Checklist for reviewing and merging a release-plz Release PR"
title: "Release Zebra (version)"
labels: "A-release, P-Critical"
assignees: ""
---

## Review the Release PR

- [ ] Version bumps are correct for each crate
- [ ] Root and per-crate CHANGELOGs read well
- [ ] If any crate shows `(⚠️ API breaking changes)`, its bump is major
- [ ] Checkpoint data is up to date (if applicable)
- [ ] End-of-support height is correct (if applicable)
- [ ] CI is green

## Merge and Monitor

- [ ] Merge the Release PR
- [ ] `release` workflow published crates and created tags
- [ ] Draft GitHub Release created with correct changelog
- [ ] Fill in the `[FILL IN]` sections and click **Publish release**
- [ ] Docker image appears on Docker Hub (`release-binaries.yml`)
- [ ] GCP deployment rolled out (`zfnd-deploy-nodes-gcp.yml`)

## If Something Goes Wrong

Re-run the failed job first. release-plz is idempotent: it recreates the Release PR, skips crates whose tags exist, and skips crates already on crates.io. The scenarios below need manual steps.

### A crate is on crates.io but no tag was created

The one case release-plz will not self-heal: on re-run it sees the crate as "done" and skips without creating the tag. Detect by comparing `cargo info <crate>` against `git ls-remote --tags origin`. Recover with:

```bash
gh release create <crate>-v<X.Y.Z> --target <release-commit-sha> --generate-notes
```

For zebrad the tag is `v<X.Y.Z>`. Promote from draft once verified.

### Roll back a bad release

1. `cargo yank --vers <X.Y.Z> <crate>` for each crate that should not be consumed.
2. Delete the GitHub Release; keep the tag (crates.io versions cannot be reused).
3. Bump to `<X.Y.Z+1>` with the fix in the next Release PR.

### Block future Release PRs during an incident

Add the `do-not-merge` label on the open Release PR.

### Emergency fallback to the old manual process

See [`release-checklist-legacy.md`](./release-checklist-legacy.md).

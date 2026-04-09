---
name: "Release Checklist"
about: "Checklist for reviewing and merging a release-plz Release PR"
title: "Release Zebra (version)"
labels: "A-release, P-Critical"
assignees: ""
---

# Review the Release PR

- [ ] Version bumps are correct for changed crates
- [ ] Root CHANGELOG.md summary is clear and user-facing
- [ ] Per-crate CHANGELOGs reflect actual API changes
- [ ] No accidental breaking changes (check cargo-semver-checks output in CI)
- [ ] Checkpoint data is up to date (if applicable)
- [ ] End-of-support height is correct (if applicable)

# Verify CI

- [ ] All CI checks pass on the Release PR
- [ ] Integration tests pass

# Merge and Monitor

- [ ] Merge the Release PR
- [ ] Verify crates published to crates.io (check release workflow output)
- [ ] Verify git tags created
- [ ] Verify GitHub Release created
- [ ] Verify Docker images built (check release-binaries workflow)
- [ ] Verify Docker Hub tag appears
- [ ] Verify GCP deployment triggered (check deploy-nodes workflow)

# If Something Goes Wrong

- Add `do-not-merge` label to block future Release PRs
- If already published: `cargo yank --vers <version> <crate>`
- Emergency fallback: See `release-checklist-legacy.md`

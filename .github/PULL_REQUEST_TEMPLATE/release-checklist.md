---
name: Release Checklist Template
about: Checklist of versioning to create a taggable commit for Zebra
title: ''
labels:
assignees: ''

---

Check:

- [ ] zebra* Cargo.tomls
- [ ] tower-* Cargo.tomls
- [ ] zebrad app version
- [ ] network protocol user agent
- [ ] README.md

- [ ] After any changes, test that the `cargo install` command in the README.md
      works (use --path instead of --git locally)

To check if any of the top-level crates need version bumps, look at the zebra
GitHub code page (landing page https://github.com/ZcashFoundation/zebra), check
to see which crates have new commits since the last version update, then deal
with dependencies.

Ideally, the branch this PR merges in will be one commit, either due to squashing and rebasing onto #main, or by only having one commit. 

Once that commit is merged in, edit the current draft release accordingly (https://keepachangelog.com/en/1.0.0/ for categorization guidance), and point it at the version bump commit (as a prerelease until we are out of alpha/beta).

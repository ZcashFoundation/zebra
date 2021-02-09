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

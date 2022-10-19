# Zebra audits

In addition to our normal [release process](https://github.com/ZcashFoundation/zebra/blob/main/book/src/dev/release-process.md], we do these steps to prepare for an audit:
1. [Tag a release candidate](https://github.com/ZcashFoundation/zebra/blob/main/book/src/dev/release-process.md#preview-releases) with the code to be audited
2. Declare a feature and fix freeze: non-essential changes must wait until after the audit, new features must be behind a [Rust feature flag](https://doc.rust-lang.org/cargo/reference/features.html)
3. Prepare a list of dependencies that are [in scope, partially in scope, and out of scope](https://github.com/ZcashFoundation/zebra/issues/5214)

This allows us to create an audit branch from the audited release candidate tag, and merge it into the `main` branch easily. Audit branches are optional, we'll make a decision based on:
- if the auditors want a separate branch to review recommended changes, and
- the complexity of the changes.

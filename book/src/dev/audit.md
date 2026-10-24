# Zebra audits

Complete [release preparation](release-process.md#release-candidate--release-process),
then fix the audit baseline and scope:

1. Select a [release candidate](release-process.md#preview-releases); record its
   immutable tag and full commit hash. Publishing a GitHub prerelease does not
   deploy it to production.
2. Declare a feature and nonessential-fix freeze for the audited scope. Defer
   unrelated changes until the audit ends. Gate new experimental features with
   [Cargo features](https://doc.rust-lang.org/cargo/reference/features.html)
   and agree which features the audit includes.
3. Give auditors the exact dependency lockfile, target platforms, enabled
   features, and scope lists. Classify dependencies and code as in scope,
   partially in scope, or out of scope, with a reason for each exclusion.
4. Prioritize Foundation-maintained production code, consensus-critical and
   security-critical paths, and code without prior audit coverage. Record prior
   audits' baselines and coverage; an audited dependency version does not cover
   later changes.
5. Agree on the channel for findings and the process for reviewing fixes.
   Handle vulnerabilities through [the security disclosure policy][security],
   not public issues containing exploit details.

Use specialist review for cryptography or consensus-critical components beyond
the primary auditors' expertise.

Use an audit branch if auditors need a stable branch for fix review or cannot
track changes on `main`. Map commits to findings and obtain auditor review before
closing them.

Publish fixes through the [release process](release-process.md#process), with
versioned changelog entries and required advisories. The [security policy][security]
treats tagged candidates as shipped releases, not unreleased code.

[security]: https://github.com/ZcashFoundation/zebra/blob/main/SECURITY.md

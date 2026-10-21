<!--
First lines: how this PR relates to issues and other PRs, one per line:
- "Fixes #123" for an issue this PR resolves
- "Part of #123: <the part it covers>" for an issue it only partly resolves
- "Closes #456: <reason>" for another PR this one replaces
- "Depends on #789: <the commits to review>" for a PR this one is stacked on
-->

## Summary

<!--
What changes and why, in at most three sentences or four short bullets. A
reviewer should understand the PR from this alone in 30 seconds, without the
code. Leave how it works to Reviewer Notes.
-->

## Reviewer Notes

<!--
Only what a reviewer needs and can't get from the diff: a design choice or
tradeoff, what users must do after the change, or an external specification.
A short paragraph each. Add a diagram, pseudocode, or table only when it shows
something faster than prose. Delete this section when there is nothing to add.
-->

## Evidence

<!--
Before and after: a test that failed and now passes, real output, or a
screenshot. Skip what CI already reports (fmt, clippy, the existing suite).
Delete this section when there is no behavior to show, such as a docs change.
-->

## Merge Danger

<!--
Door: "two-way" if reverting this PR fully undoes it, or "one-way" if it acts
outside the repository in a way a revert doesn't undo: a database format
upgrade, a publish, a production deployment, or deleted data or infrastructure.
"two-way until the next release" for consensus, peer protocol, crate API,
configuration, RPC, or metrics changes.

Blast radius: what a defect would affect, in a few words, such as "sync",
"RPC clients", "state on disk", "release path", or "CI only".
-->

**Door:**

**Blast radius:**

## AI Disclosure

<!--
The tool and what it did, for example "Claude Code wrote the implementation and
tests; I reviewed both." If you used no AI tools, say so.
-->

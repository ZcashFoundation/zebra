## Summary

<!--
The problem, and how Zebra behaves after this change, for a reviewer who hasn't
opened the code. Explain any design choice or tradeoff the diff doesn't show.
Add a pseudocode, call-tree, or file-tree sketch only when it shows something
the diff doesn't. A small change needs only a short paragraph.

Put each link on its own line:
- "Fixes #123" for an issue this PR resolves
- "Part of #123" for an issue it only partly resolves, with the part it covers
- "Closes #456" for another PR this one replaces, with a short reason, such as
  "Closes #456: supersedes it with a smaller fix"
- "Depends on #789" for a PR this one is stacked on, with the commits to review
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

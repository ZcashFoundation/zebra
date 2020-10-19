<!--
Thank you for your Pull Request.

Please provide a description above and fill in the information below.

Contributors guide: https://zebra.zfnd.org/CONTRIBUTING.html
-->

## Motivation

<!--
Explain the context and why you're making that change. What is the problem
you're trying to solve? In some cases there is not a problem and this can be
thought of as being the motivation for your change.
-->

## Solution

<!--
Summarize the solution and provide any necessary context needed to understand
the code change.
-->

## Related Issues
<!--
Please link to any existing GitHub issues pertaining to this PR.
-->

## Review Guidelines (for Reviewer)
<!--
This is a flexible checklist for the reviewer to fill in.

Developers:
Add extra tasks to the review using list items.
Skip review tasks using ~~strikethrough~~.
If you want this pull request to have a specific reviewer, tag them in the list of reviewers.

Reviewer:
This checklist can help you do your review.
Add or skip tasks as needed.
-->

**Does this pull request improve Zebra?**

- [ ] Pull Request
  - The pull request is complete: code, documentation, and tests
  - The PR contains changes to related code (unrelated changes can be split into another PR)
  - The PR does everything listed in the tickets that it closes
  - Any follow-up tasks are listed in a GitHub issue

- [ ] Code
  - The code implements the specifications or design documents
  - Changes from the specifications or design documents are explained in comments
  - The code is readable, and does not appear to have any bugs
  - Any known issues or limitations are documented

- [ ] Documentation
  - Docs summarise how the code should be used
  - Docs reference specifications or design documents
  - New public code has doc comments: `#![deny(missing_docs)]`
  - Complex private code has doc comments

- [ ] Tests
  - The tests make sure the code implements the Zcash consensus rules
  - Consensus rules have success tests, failure tests, and property tests
  - Edge cases and errors have tests
  - The consensus rules are tested on the block lists in `zebra_test::vectors`
  - Tests cover mainnet and testnet

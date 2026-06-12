# POC Scope

This page captures the boundaries for the GitBook proof of concept. It is placeholder content, but it is written in the shape of a real project decision page so maintainers can judge the layout rather than an empty scaffold.

## Goals

- Evaluate GitBook navigation with a small set of Zebra-oriented pages.
- Keep Valarbook independent from the production mdBook deployment.
- Preserve the current source of truth in `book/src` while the experiment is reviewed.
- Identify GitBook-specific formatting issues before any broader migration work.
- Deploy the rendered Valarbook site from CI when the workflow runs outside pull requests.

## Out Of Scope

- Replacing `.github/workflows/book.yml`.
- Moving internal Rust API docs into GitBook.
- Mirroring the benchmark dashboard under Valarbook.

## Promotion Criteria

The POC should only move beyond artifact preview if reviewers agree that the GitBook experience is materially better for Zebra readers. Before that happens, the team should verify that diagrams, includes, admonitions, local links, and generated documentation all have a clear home in the new structure.

## Open Questions

- Should GitBook host only user documentation, or both user and developer documentation?
- Should generated Rust documentation stay on GitHub Pages even if hand-written pages move?
- Should the POC mirror the Zcash integration-tests book structure or keep Zebra's current split between user and developer docs?

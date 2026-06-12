# Valarbook

This proof of concept is a small, committed GitBook layout for evaluating how Zebra documentation would read outside the current mdBook pipeline.

The production documentation remains in `book/src` and is still published by `.github/workflows/book.yml`. Valarbook is intentionally separate so reviewers can compare structure, navigation, and page rendering before deciding whether a broader documentation migration is useful.

## What This Preview Tests

- A GitBook-native table of contents using `SUMMARY.md`.
- A compact landing page for orienting readers.
- A user-facing page adapted from the Zcash integration-tests documentation style.
- A project-facing page that records the migration questions maintainers need answered before any larger docs change.
- A GitHub Actions workflow that builds the layout with HonKit and deploys it to GitHub Pages outside pull request runs.

## Review Checklist

- Confirm the GitBook navigation matches expectations.
- Check whether callouts, command blocks, and links render cleanly.
- Compare the reading flow with the existing Zebra mdBook site.
- Decide whether a larger content migration is worth testing.

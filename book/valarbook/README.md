# Valarbook

This proof of concept is a small, committed GitBook layout for publishing the `zebra-zcashd-compat` setup guide outside the current mdBook pipeline.

The production documentation remains in `book/src` and is still published by `.github/workflows/book.yml`. Valarbook is intentionally separate so reviewers can compare the compatibility guide's navigation and rendering before deciding whether a broader documentation migration is useful.

## What This Preview Tests

- A GitBook-native table of contents using `SUMMARY.md`.
- A compact landing page for orienting readers.
- A dedicated zebra-zcashd compatibility page adapted from the standalone setup page.
- A GitHub Actions workflow that builds the layout with HonKit and deploys it to GitHub Pages outside pull request runs.

## Review Checklist

- Confirm the GitBook navigation matches expectations.
- Check whether command blocks and links render cleanly.
- Compare the compatibility guide's reading flow with the existing Zebra mdBook site.
- Decide whether a larger content migration is worth testing.

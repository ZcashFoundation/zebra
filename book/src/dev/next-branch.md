# The `next` Branch

`next` lets maintainers run changes on nodes before they reach `main`. Each merge into `next` deploys the `next` nodes, which mirror the stage nodes deployed from `main`, so the team can compare how they sync.

## Workflow

1. Open a pull request against `next`. Use the same [conventional commit](https://www.conventionalcommits.org/en/v1.0.0/#specification) title and [change fragments](changelog-guidelines.md) as for `main`, because its commits reach `main` unchanged.
2. Merge it once the required checks pass. `next` needs no approval and has no merge queue.
3. After each push to `main`, the `sync-next.yml` workflow moves the `sync/main-to-next` branch up to `main` and opens a `chore: sync main to next` pull request from it, or updates the open one. A maintainer reviews it and merges it with a merge commit when `next` should take those commits.
4. To land the work on `main`, open a pull request from `next` into `main`. It goes through `main`'s usual review and merge queue.

## Resolve Sync Conflicts

Resolve a sync pull request's conflicts with `next` on its own branch:

```bash
git fetch origin
git switch -C sync/main-to-next origin/sync/main-to-next
git merge origin/next
# resolve the conflicts and commit the merge
git push
```

For simple text conflicts, the pull request's **Resolve conflicts** button makes the same merge in the browser. Merge the pull request once the required checks pass.

Later pushes to `main` are merged into the branch, never force-pushed over your resolution. If that merge conflicts, the workflow turns the pull request into a draft, because the branch is missing commits from `main`. Merge `origin/main` into the branch the same way, then mark the pull request ready for review.

## Publish a Pre-Release

A pre-release from `next`, such as an [alpha or a beta](release-process.md#previews), publishes what `next` runs as a GitHub pre-release with signed binaries and a Docker Hub image tagged with its version and `edge`, without publishing crates. Release candidates are cut from `main` instead.

1. Merge a `chore(release): set zebrad version to <version>` pull request into `next` that sets the `zebrad` version to a pre-release version, such as `X.Y.Z-alpha.N` or `X.Y.Z-beta.N`, and updates `Cargo.lock`. Do not use the `rc` identifier: it marks a release candidate from `main`, which deploys production. Later sync pull requests can conflict on that version: keep the higher one.
2. Generate the release notes from the pending change fragments, without committing them:

   ```bash
   changie batch v<version> --project zebrad --dry-run > release-notes.md
   ```

3. Publish the pre-release on the merge commit from step 1, with your own GitHub credentials. A release created with a workflow's `GITHUB_TOKEN` does not start the release workflows.

   ```bash
   gh release create v<version> --target <commit> --prerelease --title "Zebra <version>" --notes-file release-notes.md
   ```

Never promote it to a full release, which would make it the Latest release.

## Compare `next` With `main`

`next` nodes report to Sentry in the `next` environment, and stage nodes in `stage`. Compare nodes on the same network and zone, for example `zebrad-next-mainnet-b` and `zebrad-main-mainnet-b`.

## Read the `next` Book

The book from `next` is published at [zebra.zfnd.org/next](https://zebra.zfnd.org/next/), beside the book from `main` at [zebra.zfnd.org](https://zebra.zfnd.org/). Compare the same page in both, for example `/dev/next-branch.html` and `/next/dev/next-branch.html`.

`/next` updates after each push to `next` that changes the book. If the `next` book doesn't change for 90 days, `/next` is removed until its next change.

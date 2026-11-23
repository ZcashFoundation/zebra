# The `next` Branch

`next` lets maintainers run changes on nodes before they reach `main`. Each merge into `next` deploys the `next` nodes, which mirror the stage nodes deployed from `main`, so the team can compare how they sync.

## Workflow

1. Open a pull request against `next`. Use the same [conventional commit](https://www.conventionalcommits.org/en/v1.0.0/#specification) title and [change fragments](changelog-guidelines.md) as for `main`, because its commits reach `main` unchanged.
2. Merge it once the required checks pass. `next` needs no approval and has no merge queue.
3. After each push to `main`, the `sync-next.yml` workflow moves the `sync/main-to-next` branch up to `main` and opens a `chore: sync main to next` pull request from it, or updates the open one. A maintainer reviews it and merges it with a merge commit when `next` should take those commits.
4. To land the work on `main`, [promote `next` to `main`](#promote-next-to-main).

`next` only moves forward: through pull requests into it and through the sync pull request. Never reset or force-push it.

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

Before you start, merge any open sync pull request that changes `.github/`. A release runs the workflows of the tagged commit, so `next` needs `main`'s current release workflows.

1. Merge a `chore(release): set zebrad version to <version>` pull request into `next` that sets the `zebrad` version to a pre-release version, such as `X.Y.Z-alpha.N` or `X.Y.Z-beta.N`, and updates `Cargo.lock`. `X.Y.Z` must be above the latest `zebrad` release. Do not use the `rc` identifier: it marks a release candidate from `main`, which deploys production. Later sync pull requests can conflict on that version: keep the higher one.
2. Generate the release notes from the pending change fragments, without committing them. The notes list every fragment pending on `next`, including the ones `main` hasn't released yet, so each pre-release repeats the entries of the pre-releases before it:

   ```bash
   changie batch v<version> --project zebrad --dry-run > release-notes.md
   ```

3. Publish the pre-release on the merge commit from step 1, with your own GitHub credentials. A release created with a workflow's `GITHUB_TOKEN` does not start the release workflows.

   ```bash
   gh release create v<version> --target <commit> --prerelease --title "Zebra <version>" --notes-file release-notes.md
   ```

Never promote it to a full release, which would make it the Latest release.

## Promote `next` to `main`

A promotion merges everything `next` runs into `main`, so the next release from `main` includes it.

1. Create a `promote/` branch from `origin/next`:

   ```bash
   git fetch origin
   git switch -c promote/next-to-main origin/next
   ```

2. If `next` carries a `zebrad` pre-release version, add a commit that sets `zebrad` back to its version on `main` and updates `Cargo.lock`. The release workflow stops before it opens the Release PR when the `zebrad` version is above the one on crates.io and its tag already exists. The [change fragments](release-process.md) then plan the release version.
3. Open a `chore: promote next to main` pull request into `main`, and review the whole difference between the branches, not only the last commit. Its title takes no `!`, even when the branch adds `breaking` fragments: the fragments plan the versions, and the PR Gate tolerates the `changelog-gate` failure.
4. Hold the sync pull request until the Release PR that follows the promotion merges. The promotion commit from step 2 is on `main` until then, so an earlier sync would move `zebrad` on `next` back to that older version.
5. After the release, merge the sync pull request. The next pre-release from `next` takes a version above that release.

## Change Fragments on `next`

Pull requests into `next` add [change fragments](changelog-guidelines.md) exactly as on `main`. The fragments stay in `.changes/unreleased/` until a release from `main` consumes them:

- A pre-release from `next` consumes no fragments, because its notes come from a dry run.
- A release from `main` removes the fragments it consumed and adds a `.changes/<project>/vX.Y.Z.md` version file. The sync pull request carries both changes to `next`.
- Fragments that exist only on `next` stay pending until a promotion takes them to `main`.

A change that reaches `main` through its own pull request, instead of through `next`'s commits, comes with a new fragment under a new file name. After the next sync, `next` has two fragments for the same change. Delete `next`'s copy in a pull request into `next`. Otherwise the change appears twice in the release notes. Before a promotion, check that no pending fragment on `next` describes a change that a version file under `.changes/` already lists.

## Compare `next` With `main`

`next` nodes report to Sentry in the `next` environment, and stage nodes in `stage`. Compare nodes on the same network and zone, for example `zebrad-next-mainnet-b` and `zebrad-main-mainnet-b`.

## Read the `next` Book

The book from `next` is published at [zebra.zfnd.org/next](https://zebra.zfnd.org/next/), beside the book from `main` at [zebra.zfnd.org](https://zebra.zfnd.org/). Compare the same page in both, for example `/dev/next-branch.html` and `/next/dev/next-branch.html`.

`/next` updates after each push to `next` that changes the book. If the `next` book doesn't change for 90 days, `/next` is removed until its next change.

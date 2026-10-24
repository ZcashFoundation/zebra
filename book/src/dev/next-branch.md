# The `next` branch

The `next` branch deploys changes before merging them into `main`, under
the [deployment triggers and environments](continuous-delivery.md#triggers-and-environments).

## Workflow

1. Open a pull request against `next`. Use the same
   [conventional commit](https://www.conventionalcommits.org/en/v1.0.0/#specification)
   title and [change fragments](changelog-guidelines.md) required for
   `main`, because merge commits preserve the branch's commits.
2. Merge after the required checks pass. The
   [next branch ruleset](https://github.com/ZcashFoundation/zebra/rules/25572141)
   requires `lint`, `unit-tests`, `test-crates`, `pr-gate-result`, and
   `merge-policy`. It requires a PR and merge commits, but no approving
   review or merge queue. It does not require `mergefreeze`.
3. Keep `next` current by reviewing and merging the automatic
   `sync/main-to-next` PR with a merge commit when `next` should take those
   commits. Resolve conflicts on that PR's own branch as below.
4. To promote the branch, open a PR from `next` to `main`. It follows
   `main`'s [review and merge-queue requirements](continuous-integration.md#automated-merges).
   Review the entire branch difference, not only the latest change.

Dispatch [GCP stateful tests](continuous-integration.md#gcp-stateful-tests)
manually for `next`; they do not run on its PRs or gate its merges.

## Synchronization from main

The [sync workflow](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/sync-next.yml)
runs on every `main` push and manual dispatch from `main`. Its `sync` job
uses `ubuntu-26.04` with a five-minute timeout and:

- Exits successfully if `next` already contains every commit from `main`.
- Creates or advances `sync/main-to-next` to `main`, or merges `main` into
  that branch when a maintainer's conflict resolution prevents a fast-forward.
  It never force-pushes over a resolution.
- Finds an open PR from this repository's `sync/main-to-next` to `next`,
  rather than accepting a fork's branch with the same name, or creates
  `chore: sync main to next` if none exists.
- Makes the PR a draft if merging newer `main` commits into its branch
  conflicts. It reports conflicts with `next` but does not resolve them or
  merge the PR automatically.

The job opens PRs with a GitHub App token from `RELEASE_APP_ID` and
`RELEASE_APP_PRIVATE_KEY`, requesting contents-write and pull-requests-write
permissions. Using `GITHUB_TOKEN` would not trigger required checks; the
job's ordinary GitHub token has no permissions.

## Resolve sync conflicts

Resolve a sync PR's conflicts with `next` on its own branch. The example
assumes `origin` points to the Zebra repository:

```sh
git fetch origin
git switch -C sync/main-to-next origin/sync/main-to-next
git merge origin/next
# Resolve each conflict, stage the files, and complete the merge commit.
git push
```

For simple text conflicts, the PR's **Resolve conflicts** button makes the
same merge in the browser. Merge the PR once the required checks pass.

Later pushes to `main` are merged into the branch, never force-pushed over
your resolution. If that merge conflicts, the workflow makes the PR a draft
because its branch is missing commits from `main`. Merge `origin/main` into
the branch the same way, then mark the PR ready for review. Do not force-push
or reset `next` to discard the changes under evaluation.

## Publish a pre-release

A prerelease from `next`, such as an [alpha or a beta](release-process.md#previews),
publishes what `next` runs as a GitHub prerelease with signed binaries and a
Docker Hub image tagged with its version and `edge`, without publishing
crates. Release candidates are cut from `main` instead.

1. Merge a `chore(release): set zebrad version to <version>` PR into `next`
   that sets the `zebrad` version to a prerelease such as `X.Y.Z-alpha.N` or
   `X.Y.Z-beta.N`, and updates `Cargo.lock`. Do not use the `rc` identifier:
   it marks a release candidate from `main`. No prerelease publication deploys
   production. Later sync PRs can conflict on that version: keep the higher one.
2. Generate release notes from the pending change fragments without
   committing them:

   ```sh
   changie batch v<version> --project zebrad --dry-run > release-notes.md
   ```

3. Publish the prerelease on the merge commit from step 1 with your own
   GitHub credentials. A release created with a workflow's `GITHUB_TOKEN`
   does not start release workflows.

   ```sh
   gh release create v<version> --target <commit> --prerelease --title "Zebra <version>" --notes-file release-notes.md
   ```

Never promote it to a full release, which would make it the Latest release.

## Compare next with main

Compare nodes on the same network and zone, such as
`zebrad-next-mainnet-b` and `zebrad-main-mainnet-b`. Check actual deployed
commits and health, not branch tips.

Normal next deployments report Sentry environment `next`; main pushes
report `stage`. Both [record Sentry deployments](gcp-deployment-operations.md#sentry-releases).
For equivalent comparisons, avoid [manual dispatches](gcp-deployment-operations.md#run-one)
selecting `dev`, even when they target the same resource names.

Use the [deployment runbook](gcp-deployment-operations.md) for per-zone
inspection and deployment. Follow its
[disk compatibility procedure](gcp-deployment-operations.md#db-format-version-break-release)
before testing a state-format change.

## Read the `next` book

The book from `next` is published at [zebra.zfnd.org/next](https://zebra.zfnd.org/next/),
beside the book from `main` at [zebra.zfnd.org/main](https://zebra.zfnd.org/main/).
The site root documents the latest release. Compare the same page in both
development books, for example `/main/dev/next-branch.html` and
`/next/dev/next-branch.html`.

`/next` updates after each push to `next` that changes the book. If the `next`
book does not change for 90 days, `/next` is removed until its next change.

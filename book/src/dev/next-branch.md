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
3. Keep `next` current through the automatic `main`-to-`next` sync PR,
   which merges itself with a merge commit once its checks pass. To hold
   it, add the `do-not-merge` label. Resolve conflicts on its branch as
   below.
4. To promote the branch, open a PR from `next` to `main`. It follows
   `main`'s [review and merge-queue requirements](continuous-integration.md#automated-merges).
   Review the entire branch difference, not only the latest change.

Dispatch [GCP stateful tests](continuous-integration.md#gcp-stateful-tests)
manually for `next`; they do not run on its PRs or gate its merges.

## Synchronization from main

The [sync workflow](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/sync-next.yml)
runs on every `main` push and on manual dispatch from `main`. Its `sync` job
uses `ubuntu-26.04` with a five-minute timeout and:

- Exits successfully if `next` already contains every commit from `main`.
- Moves the `sync/main-to-next` branch up to `main`: it creates the branch,
  fast-forwards it, or, once a maintainer's conflict resolution blocks the
  fast-forward, merges `main` into it. It never force-pushes over that
  resolution.
- Finds an open PR from this repository's `sync/main-to-next` into `next`,
  rather than accepting a fork's branch with the same name, and creates
  `chore: sync main to next` if none exists.
- Enables auto-merge with a merge commit when the PR is mergeable. A
  conflicting PR can't merge itself; the run summary reports the conflict.

The job opens PRs with a GitHub App token from `RELEASE_APP_ID` and
`RELEASE_APP_PRIVATE_KEY`, requesting contents-write and pull-requests-write
permissions. Using `GITHUB_TOKEN` would not trigger required checks; the
job's ordinary GitHub token has no permissions.

## Resolve sync conflicts

Resolve conflicts on the sync PR's own branch. The example assumes `origin`
points to the Zebra repository:

```sh
git fetch origin
git switch -C sync/main-to-next origin/sync/main-to-next
git merge origin/next
```

Resolve each conflict, stage the resolved files, complete the merge commit,
and push it with `git push`. For simple text conflicts, the PR's
**Resolve conflicts** button makes the same merge in the browser. Merge the
PR with a merge commit after its checks pass. Do not force-push or reset
`next` to discard the changes under evaluation.

Later pushes to `main` are merged into the branch, never force-pushed over
your resolution. If that merge conflicts, merge `origin/main` into the
branch the same way.

## Publish a pre-release

A pre-release from `next`, such as an [alpha or a beta](release-process.md#preview-releases),
publishes what `next` runs as a GitHub pre-release with signed binaries and
a Docker Hub image tagged with its version and `edge`, without publishing
crates or deploying production. Release candidates are cut from `main`
instead.

1. Merge a `chore(release): set zebrad version to <version>` PR into `next`
   that sets the `zebrad` version to a pre-release version, such as
   `X.Y.Z-alpha.N` or `X.Y.Z-beta.N`, and updates `Cargo.lock`. Do not use
   the `rc` identifier: it marks a release candidate from `main`. Later sync
   PRs can conflict on that version: keep the higher one.
2. Generate the release notes from the pending change fragments, without
   committing them:

   ```sh
   changie batch v<version> --project zebrad --dry-run > release-notes.md
   ```

3. Publish the pre-release on the merge commit from step 1, with your own
   GitHub credentials. A release created with a workflow's `GITHUB_TOKEN`
   does not start the release workflows.

   ```sh
   gh release create v<version> --target <commit> --prerelease \
     --title "Zebra <version>" --notes-file release-notes.md
   ```

Never promote it to a full release: that would make it the Latest release
and trigger a production deployment.
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

## Read the next book

The book from `next` is published at [zebra.zfnd.org/next](https://zebra.zfnd.org/next/),
beside the latest release's book at [zebra.zfnd.org](https://zebra.zfnd.org/)
and `main`'s at [zebra.zfnd.org/main](https://zebra.zfnd.org/main/). Compare
the same page in each, for example `/main/dev/next-branch.html` and
`/next/dev/next-branch.html`.

`/next` updates after each push to `next` that changes the book. If the
`next` book doesn't change for 90 days, `/next` is removed until its next
change.

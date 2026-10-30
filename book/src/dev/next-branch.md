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
3. Keep `next` current by merging the automatic `main`-to-`next` sync PR
   with a merge commit. Resolve conflicts on a separate branch as below.
4. To promote the branch, open a PR from `next` to `main`. It follows
   `main`'s [review and merge-queue requirements](continuous-integration.md#automated-merges).
   Review the entire branch difference, not only the latest change.

Dispatch [GCP stateful tests](continuous-integration.md#gcp-stateful-tests)
manually for `next`; they do not run on its PRs or gate its merges.

## Synchronization from main

The [sync workflow](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/sync-next.yml)
runs on every `main` push and manual dispatch. Its `sync` job uses
`ubuntu-26.04` with a five-minute timeout and:

- Exits successfully if `next` does not exist or already contains every
  commit from `main`.
- Finds an open PR whose head is this repository's `main` and base is
  `next`, rather than accepting a fork's branch with the same name.
- Creates `chore: sync main to next` if that PR does not exist. An existing
  PR picks up subsequent commits through its `main` head.
- Reports conflicting mergeability in the run summary. It does not resolve
  conflicts or merge the PR automatically.

The job opens PRs with a GitHub App token from `RELEASE_APP_ID` and
`RELEASE_APP_PRIVATE_KEY`, requesting contents-read and pull-requests-write
permissions. Using `GITHUB_TOKEN` would not trigger required checks; the
job's ordinary GitHub token has no permissions.

## Resolve sync conflicts

The automatic PR's head is `main`; do not commit conflict resolutions
directly to that branch. Start from `next` in a clean checkout and merge
`main` into a new branch. The example assumes `origin` points to the Zebra
repository:

```sh
git fetch origin
git switch -c "sync/main-to-next-$(date -u +%Y%m%d-%H%M%S)" origin/next
git merge origin/main
```

Resolve each conflict, stage the resolved files, and complete the merge
commit. Then push and open a PR:

```sh
git push -u origin HEAD
gh pr create --repo ZcashFoundation/zebra --base next \
  --title "chore: sync main to next"
```

Merge that PR with a merge commit after its checks pass. Recheck the
automatic sync PR: if it still contains newer commits from `main`, merge
those separately; if it is redundant, close it. Do not force-push or reset
`next` to discard the changes under evaluation.

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

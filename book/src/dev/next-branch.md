# The `next` Branch

`next` lets maintainers run changes on nodes before they reach `main`. Each merge into `next` deploys the `next` nodes, which mirror the stage nodes deployed from `main`, so the team can compare how they sync.

## Workflow

1. Open a pull request against `next`. Use the same [conventional commit](https://www.conventionalcommits.org/en/v1.0.0/#specification) title and [change fragments](changelog-guidelines.md) as for `main`, because its commits reach `main` unchanged.
2. Merge it once the required checks pass. `next` needs no approval and has no merge queue.
3. After each push to `main`, the `sync-next.yml` workflow opens a `chore: sync main to next` pull request. Merge it with a merge commit.
4. To land the work on `main`, open a pull request from `next` into `main`. It goes through `main`'s usual review and merge queue.

## Resolve Sync Conflicts

The sync pull request's head is `main`, so resolve its conflicts on a branch cut from `next`:

```bash
git fetch origin
git switch -c sync/main-to-next-$(date +%Y%m%d) origin/next
git merge origin/main
# resolve the conflicts and commit the merge
git push -u origin HEAD
gh pr create --base next --title "chore: sync main to next"
```

Merge it with a merge commit. GitHub then marks the automatic sync pull request as merged, or leaves it open with only newer commits.

## Compare `next` With `main`

`next` nodes report to Sentry in the `next` environment, and stage nodes in `stage`. Compare nodes on the same network and zone, for example `zebrad-next-mainnet-b` and `zebrad-main-mainnet-b`.

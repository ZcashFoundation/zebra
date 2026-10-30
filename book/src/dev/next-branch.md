# The `next` Branch

`next` lets maintainers run changes on nodes before they reach `main`. Each merge into `next` deploys the `next` nodes, which mirror the stage nodes deployed from `main`, so the team can compare how they sync.

## Workflow

1. Open a pull request against `next`. Use the same [conventional commit](https://www.conventionalcommits.org/en/v1.0.0/#specification) title and [change fragments](changelog-guidelines.md) as for `main`, because its commits reach `main` unchanged.
2. Merge it once the required checks pass. `next` needs no approval and has no merge queue.
3. After each push to `main`, the `sync-next.yml` workflow moves the `sync/main-to-next` branch up to `main` and opens a `chore: sync main to next` pull request from it, or updates the open one. The pull request merges itself with a merge commit once the required checks pass. To hold it, add the `do-not-merge` label.
4. To land the work on `main`, open a pull request from `next` into `main`. It goes through `main`'s usual review and merge queue.

## Resolve Sync Conflicts

A sync pull request that conflicts with `next` can't merge itself. Resolve the conflicts on its own branch:

```bash
git fetch origin
git switch -C sync/main-to-next origin/sync/main-to-next
git merge origin/next
# resolve the conflicts and commit the merge
git push
```

For simple text conflicts, the pull request's **Resolve conflicts** button makes the same merge in the browser. Merge the pull request once the required checks pass.

Later pushes to `main` are merged into the branch, never force-pushed over your resolution. If that merge conflicts, merge `origin/main` into the branch the same way.

## Compare `next` With `main`

`next` nodes report to Sentry in the `next` environment, and stage nodes in `stage`. Compare nodes on the same network and zone, for example `zebrad-next-mainnet-b` and `zebrad-main-mainnet-b`.

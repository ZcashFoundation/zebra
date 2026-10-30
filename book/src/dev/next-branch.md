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

## Read the `next` Book

The book from `next` is published at [zebra.zfnd.org/next](https://zebra.zfnd.org/next/), beside the book from `main` at [zebra.zfnd.org](https://zebra.zfnd.org/). Compare the same page in both, for example `/dev/next-branch.html` and `/next/dev/next-branch.html`.

A push to `next` that changes the book builds it in `book-next.yml`. When that build succeeds, `book.yml` on `main` publishes it, because GitHub Pages publishes the whole site in one deployment and only `main` may deploy it. Every later docs deploy from `main` keeps the newest `next` build at `/next`. If a `next` build fails, `/next` keeps the previous one. GitHub deletes a build 90 days after it runs, so `/next` disappears if the `next` book doesn't change for 90 days, until its next change.

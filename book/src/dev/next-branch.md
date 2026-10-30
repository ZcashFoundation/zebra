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

## Release an Alpha

An [alpha](release-process.md#previews) publishes what `next` runs as a GitHub pre-release with signed binaries and a Docker Hub image tagged with its version and `edge`, without publishing crates.

1. Merge a `chore(release): set zebrad version to <version>` pull request into `next` that sets the `zebrad` version, such as `8.1.0-alpha.0`, and updates `Cargo.lock`. Later sync pull requests can conflict on that version: keep the higher one.
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

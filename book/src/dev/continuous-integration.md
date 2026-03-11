# Zebra Continuous Integration

## Overview

Zebra has extensive continuous integration tests for node syncing and `lightwalletd` integration.

On every PR change, Zebra runs [these Docker tests](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/ci-integration-tests-gcp.yml):
- Zebra update syncs from a cached state Google Cloud tip image
- lightwalletd full syncs from a cached state Google Cloud tip image
- lightwalletd update syncs from a cached state Google Cloud tip image
- lightwalletd integration with Zebra JSON-RPC and Light Wallet gRPC calls

When a PR is merged to the `main` branch, we also run a Zebra full sync test from genesis.
Some of our builds and tests are repeated on the `main` branch, due to:
- GitHub's cache sharing rules,
- our cached state sharing rules, or
- generating base coverage for PR coverage reports.

Currently, each Zebra and lightwalletd full and update sync will update cached state images,
which are shared by all tests. Tests prefer the latest image generated from the same commit.
But if a state from the same commit is not available, tests will use the latest image from
any branch and commit, as long as the state version is the same.

Zebra also does [a smaller set of tests](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/ci-unit-tests-os.yml) on tier 2 platforms using GitHub actions runners.

## Automated Merges

We use [Mergify](https://dashboard.mergify.com/github/ZcashFoundation/repo/zebra/queues) to automatically merge most pull requests.
To merge, a PR has to pass all required `main` branch protection checks, and be approved by a Zebra developer.

We try to use Mergify as much as we can, so all PRs get consistent checks.

Some PRs don't use Mergify:
- Mergify config updates
- Admin merges, which happen when there are multiple failures on the `main` branch
- Manual merges (these are allowed by our branch protection rules, but we almost always use Mergify)

Merging with failing CI is usually disabled by our branch protection rules.
See the `Admin: Manually Merging PRs` section below for manual merge instructions.

We use workflow conditions to skip some checks on PRs, Mergify, or the `main` branch.
For example, some workflow changes skip Rust code checks. When a workflow can skip a check, we need to create [a patch workflow](https://docs.github.com/en/pull-requests/collaborating-with-pull-requests/collaborating-on-repositories-with-code-quality-features/troubleshooting-required-status-checks#handling-skipped-but-required-checks)
with an empty job with the same name. This is a [known Actions issue](https://github.com/orgs/community/discussions/13690#discussioncomment-6653382).
This lets the branch protection rules pass when the job is skipped. In Zebra, we name these workflows with the extension `.patch.yml`.

### Branch Protection Rules

Branch protection rules should be added for every failure that should stop a PR merging, break a release, or cause problems for Zebra users.
We also add branch protection rules for developer or devops features that we need to keep working, like coverage.

But the following jobs don't need branch protection rules:
* Testnet jobs: testnet is unreliable.
* Optional linting jobs: some lint jobs are required, but some jobs like spelling and actions are optional.
* Jobs that rarely run: for example, cached state rebuild jobs.
* Setup jobs that will fail another later job which always runs, for example: Google Cloud setup jobs.
  We have branch protection rules for build jobs, but we could remove them if we want.

When a new job is added in a PR, use the `#devops` Slack channel to ask a GitHub admin to add a branch protection rule after it merges.
Adding a new Zebra crate automatically adds a new job to build that crate by itself in [ci-build-crates.yml](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/ci-build-crates.yml),
so new crate PRs also need to add a branch protection rule.

#### Admin: Changing Branch Protection Rules

[Zebra repository admins](https://github.com/orgs/ZcashFoundation/teams/zebra-admins) and
[Zcash Foundation organisation owners](https://github.com/orgs/ZcashFoundation/people?query=role%3Aowner)
can add or delete branch protection rules in the Zebra repository.

To change branch protection rules:

Any developer:

0. Run a PR containing the new rule, so its name is available to autocomplete.
1. If the job doesn't run on all PRs, add a patch job with the name of the job.
   If the job calls a reusable workflow, the name is `Caller job / Reusable step`.
   (The name of the job inside the reusable workflow is ignored.)

Admin:

2. Go to the [branch protection rule settings](https://github.com/ZcashFoundation/zebra/settings/branches)
3. Click on `Edit` for the `main` branch
4. Scroll down to the `Require status checks to pass before merging` section.
   (This section must always be enabled. If it is disabled, all the rules get deleted.)

To add jobs:

5. Start typing the name of the job or step in the search box
6. Select the name of the job or step to add it

To remove jobs:

7. Go to `Status checks that are required.`
8. Find the job name, and click the cross on the right to remove it

And finally:

9. Click `Save changes`, using your security key if needed

If you accidentally delete a lot of rules, and you can't remember what they were, ask a
ZF organisation owner to send you a copy of the rules from the [audit log](https://github.com/organizations/ZcashFoundation/settings/audit-log).

Organisation owners can also monitor rule changes and other security settings using this log.

#### Admin: Manually Merging PRs

Admins can allow merges with failing CI, to fix CI when multiple issues are causing failures.

Admin:
1. Follow steps 2 and 3 above to open the `main` branch protection rule settings
2. Scroll down to `Do not allow bypassing the above settings`
3. Uncheck it
4. Click `Save changes`
5. Do the manual merge, and put an explanation on the PR
6. Re-open the branch protection rule settings, and re-enable `Do not allow bypassing the above settings`


### Pull Requests from Forked Repositories

GitHub doesn't allow PRs from forked repositories to have access to our repository secret keys, even after we approve their CI.
This means that Google Cloud CI fails on these PRs.

Until we [fix this CI bug](https://github.com/ZcashFoundation/zebra/issues/4529), we can merge external PRs by:
1. Reviewing the code to make sure it won't give our secret keys to anyone
2. Pushing a copy of the branch to the Zebra repository
3. Opening a PR using that branch
4. Closing the original PR with a note that it will be merged (closing duplicate PRs is required by Mergify)
5. Asking another Zebra developer to approve the new PR

## Manual Testing Using Google Cloud

Some Zebra developers have access to the Zcash Foundation's Google Cloud instance, which also runs our automatic CI.

Please shut down large instances when they are not being used.

### Automated Deletion

The [Delete GCP Resources](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/delete-gcp-resources.yml)
workflow automatically deletes test instances, instance templates, disks, and images older than a few days.

If you want to keep instances, instance templates, disks, or images in Google Cloud, name them so they don't match the automated names:
- deleted instances, instance templates and disks end in a commit hash, so use a name that doesn't end in `-[0-9a-f]{7,}`
- deleted disks and images start with `zebrad-` or `lwd-`, so use a name starting with anything else

Our production Google Cloud project doesn't have automated deletion.

## Troubleshooting

To improve CI performance, some Docker tests are stateful.

Tests can depend on:
- built Zebra and `lightwalletd` docker images
- cached state images in Google cloud
- jobs that launch Google Cloud instances for each test
- multiple jobs that follow the logs from Google Cloud (to work around the 6 hour GitHub actions limit)
- a final "Run" job that checks the exit status of the Rust acceptance test
- the current height and user-submitted transactions on the blockchain, which changes every minute

To support this test state, some Docker tests depend on other tests finishing first.
This means that the entire workflow must be re-run when a single test fails.

### Finding Errors

0. Check if the same failure is happening on the `main` branch or multiple PRs.
   If it is, open a ticket and tell the Zebra team lead.

1. Look for the earliest job that failed, and find the earliest failure.

For example, this failure doesn't tell us what actually went wrong:
>  Error: The template is not valid. ZcashFoundation/zebra/.github/workflows/zfnd-build-docker-image.yml@8bbc5b21c97fafc83b70fbe7f3b5e9d0ffa19593 (Line: 52, Col: 19): Error reading JToken from JsonReader. Path '', line 0, position 0.

https://github.com/ZcashFoundation/zebra/runs/8181760421?check_suite_focus=true#step:41:4

But the specific failure is a few steps earlier:
>  #24 2117.3 error[E0308]: mismatched types
>  ...

https://github.com/ZcashFoundation/zebra/runs/8181760421?check_suite_focus=true#step:8:2112

2. The earliest failure can also be in another job or pull request:
  a. check the whole workflow run (use the "Summary" button on the top left of the job details, and zoom in)
  b. if Mergify failed with "The pull request embarked with main cannot be merged", look at the PR "Conversation" tab, and find the latest Mergify PR that tried to merge this PR. Then start again from step 1.

3. If that doesn't help, try looking for the latest failure. In Rust tests, the "failure:" notice contains the failed test names.

### Fixing CI Sync Timeouts

CI sync jobs near the tip will take different amounts of time as:
- the blockchain grows, and
- Zebra's checkpoints are updated.

To fix a CI sync timeout, follow these steps until the timeouts are fixed:
1. Check for recent PRs that could have caused a performance decrease
2. [Update Zebra's checkpoints](https://github.com/ZcashFoundation/zebra/blob/main/zebra-utils/README.md#zebra-checkpoints)
3. If a Rust test fails with "command did not log any matches for the given regex, within the ... timeout":

    a. If it's the full sync test, [increase the full sync timeout](https://github.com/ZcashFoundation/zebra/pull/5129/files)

    b. If it's an update sync test, [increase the update sync timeouts](https://github.com/ZcashFoundation/zebra/commit/9fb87425b76ba3747985ea2f22043ff0276a03bd#diff-92f93c26e696014d82c3dc1dbf385c669aa61aa292f44848f52167ab747cb6f6R51)

### Fixing Duplicate Dependencies in `Check deny.toml bans`

Zebra's CI checks for duplicate crate dependencies: multiple dependencies on different versions of the same crate.
If a developer or dependabot adds a duplicate dependency, the `Check deny.toml bans` CI job will fail.

You can view Zebra's entire dependency tree using `cargo tree`. It can also show the active features on each dependency.

To fix duplicate dependencies, follow these steps until the duplicate dependencies are fixed:

1. Check for updates to the crates mentioned in the `Check deny.toml bans` logs, and try doing them in the same PR.
   For an example, see [PR #5009](https://github.com/ZcashFoundation/zebra/pull/5009#issuecomment-1232488943).

   a. Check for open dependabot PRs, and

   b. Manually check for updates to those crates on https://crates.io .

2. If there are still duplicate dependencies, try removing those dependencies by disabling crate features:

   a. Check for features that Zebra activates in its `Cargo.toml` files, and try turning them off, then

   b. Try adding `default-features = false` to Zebra's dependencies (see [PR #4082](https://github.com/ZcashFoundation/zebra/pull/4082/files)).

3. If there are still duplicate dependencies, add or update `skip-tree` in [`deny.toml`](https://github.com/ZcashFoundation/zebra/blob/main/deny.toml):

   a. Prefer exceptions for dependencies that are closer to Zebra in the dependency tree (sometimes this resolves other duplicates as well),

   b. Add or update exceptions for the earlier version of duplicate dependencies, not the later version, and

   c. Add a comment about why the dependency exception is needed: what was the direct Zebra dependency that caused it?

   d. For an example, see [PR #4890](https://github.com/ZcashFoundation/zebra/pull/4890/files).

4. Repeat step 3 until the dependency warnings are fixed. Adding a single `skip-tree` exception can resolve multiple warnings.

#### Fixing "unmatched skip root" warnings in `Check deny.toml bans`

1. Run `cargo deny --all-features check bans`, or look at the output of the latest "Check deny.toml bans --all-features" job on the `main` branch

2. If there are any "skip tree root was not found in the dependency graph" warnings, delete those versions from `deny.toml`

### Fixing Disk Full Errors

If the Docker cached state disks are full, increase the disk sizes in:
- [deploy-gcp-tests.yml](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/deploy-gcp-tests.yml)
- [zfnd-deploy-nodes-gcp.yml](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zfnd-deploy-nodes-gcp.yml)

If the GitHub Actions disks are full, follow these steps until the errors are fixed:

0. Check if error is also happening on the `main` branch. If it is, skip the next step.
1. Update your branch to the latest `main` branch, this builds with all the latest dependencies in the `main` branch cache.
2. Clear the GitHub Actions code cache for the failing branch. Code caches are named after the compiler version.
3. Clear the GitHub Actions code caches for all the branches and the `main` branch.

These errors often happen after a new compiler version is released, because the caches can end up with files from both compiler versions.

You can find a list of caches using:
```sh
gh api -H "Accept: application/vnd.github+json" repos/ZcashFoundation/Zebra/actions/caches
```

And delete a cache by `id` using:
```sh
gh api --method DELETE -H "Accept: application/vnd.github+json" /repos/ZcashFoundation/Zebra/actions/caches/<id>
```

These commands are from the [GitHub Actions Cache API reference](https://docs.github.com/en/rest/actions/cache).

### Retrying After Temporary Errors

Some errors happen due to network connection issues, high load, or other rare situations.

If it looks like a failure might be temporary, try re-running all the jobs on the PR using one of these methods:
1. `@mergifyio update`
2. `@dependabot recreate` (for dependabot PRs only)
3. click on the failed job, and select "re-run all jobs". If the workflow hasn't finished, you might need to cancel it, and wait for it to finish.

Here are some of the rare and temporary errors that should be retried:
- Docker: "buildx failed with ... cannot reuse body, request must be retried"
- Failure in `local_listener_fixed_port_localhost_addr_v4` Rust test, mention [ticket #4999](https://github.com/ZcashFoundation/zebra/issues/4999) on the PR
- any network connection or download failures

We track some rare errors using tickets, so we know if they are becoming more common and we need to fix them.

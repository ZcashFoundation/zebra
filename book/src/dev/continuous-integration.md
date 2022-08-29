# Zebra Continuous Integration

## Overview

Zebra has extensive continuous integration tests for node syncing and `lightwalletd` integration.

On every PR change, Zebra runs [these Docker tests](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/continous-integration-docker.yml):
- Zebra update syncs from a cached state Google Cloud tip image
- lightwalletd full syncs from a cached state Google Cloud tip image
- lightwalletd update syncs from a cached state Google Cloud tip image
- lightwalletd integration with Zebra JSON-RPC and Light Wallet gRPC calls

When a PR is merged to the `main` branch, we also run a Zebra full sync test from genesis.

Currently, each Zebra and lightwalletd full and update sync will updates cached state images,
which are shared by all tests. Tests prefer the latest image generated from the same commit.
But if a state from the same commit is not available, tests will use the latest image from
any branch and commit, as long as the state version is the same.

Zebra also does [a smaller set of tests](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/continous-integration-os.yml) on tier 2 platforms using GitHub actions runners.


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

### Resolving CI Sync Timeouts

CI sync jobs near the tip will take different amounts of time as:
- the blockchain grows, and
- Zebra's checkpoints are updated.

To resolve a CI sync timeout:
1. Check for recent PRs that could have caused a performance decrease
2. [Update Zebra's checkpoints](https://github.com/ZcashFoundation/zebra/blob/main/zebra-utils/README.md#zebra-checkpoints)
3. Wait for a full or update sync to finish with the new checkpoints
4. The GitHub actions job limit is 6 hours, so the ideal job time is 4-5 hours.
   If any GitHub actions job times out, or takes over 5 hours:
    a. [Split the job based on the sync height](https://github.com/ZcashFoundation/zebra/pull/4961/files#diff-4c3718f100312ddc9472f5d4ab2ee0a50a46f2af21352a25fca849734e3f7514R732), or
    b. Adjust the sync heights in existing jobs.
5. If a Rust test fails with "command did not log any matches for the given regex, within the ... timeout":
   a. If it's the full sync test, [increase the full sync timeout](https://github.com/ZcashFoundation/zebra/commit/9fb87425b76ba3747985ea2f22043ff0276a03bd#diff-8fbc73b0a92a4f48656ffe7d85d55c612c755202dcb7284d8f6742a38a6e9614R367)
   b. If it's an update sync test, [increase the update sync timeouts](https://github.com/ZcashFoundation/zebra/commit/9fb87425b76ba3747985ea2f22043ff0276a03bd#diff-92f93c26e696014d82c3dc1dbf385c669aa61aa292f44848f52167ab747cb6f6R51)

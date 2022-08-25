# Zebra Continuous Integration

Zebra has extensive continuous integration tests for node syncing and `lightwalletd` integration.

On every PR change, Zebra runs [these Docker tests](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/continous-integration-docker.yml):
- Zebra update syncs from a cached state Google Cloud tip image
- lightwalletd full syncs from a cached state Google Cloud tip image
- lightwalletd update syncs from a cached state Google Cloud tip image
- lightwalletd integration with Zebra JSON-RPC and Light Wallet gRPC calls

The Docker tests are stateful, they depend on jobs that launch Google Cloud instances for each test.
Some tests depend on other tests finishing first.

Currently, each Zebra and lightwalletd sync updates the cached images, which are shared by all tests.
Tests prefer the latest image generated from the same branch and commit. But if they are not available, they will use the latest image from any branch and commit, as long as the state version is the same.

Zebra also does [a smaller set of tests](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/continous-integration-os.yml) on tier 2 platforms using GitHub actions runners.


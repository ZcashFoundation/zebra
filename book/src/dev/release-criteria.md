# Go/No-Go Release Criteria

## Alpha Release

In the first alpha release, we want Zebra to participate in the network and replicate the Zcash chain state. We also want to validate proof of work and the transaction merkle tree, and serve blocks to peers.

### System Requirements

Our CI tests that Zebra builds, passes its tests, and syncs to tip on the following configurations:
* Ubuntu Linux ...

Our CI tests that Zebra builds and passes its tests on the following configurations:
* recent macOS ...
* recent Windows ...

### Build Requirements

Our CI builds Zebra with:
* Rust stable
* recent llvm
* recent clang
* recent libclang

### Supported Platforms

While Zebra is still in alpha, we don't guarantee support for any particular platforms.

But you'll probably get the best results with a recent Ubuntu Linux, or the other platforms that our CI runs on.

### Go/No-Go Status: 🛑

_Last updated: November 30 2020_

- `zebrad` Functionality
    - `zebrad` can sync to mainnet tip
        - ⚠️ under excellent network conditions (within 2 - 5 hours)
        - _reasonable and sub-optimal network conditions are not yet supported_
    - `zebrad` can stay within a few blocks of the mainnet tip after the initial sync
        - ⚠️ under excellent network conditions
        - _reasonable and sub-optimal network conditions are not yet supported_
    - ⚠️  `zebrad` can validate proof of work
    - 🛑 `zebrad` can validate the transaction merkle tree
    - ⚠️ `zebrad` can serve blocks to peers
- `zebrad` Performance
    - ⚠️ `zebrad` functionality works on platforms that meet its system requirements
- Testing
    - ⚠️ CI Passes
        - ✅  Unit tests pass reliably
        - ✅  Property tests pass reliably
        - ⚠️ Acceptance tests pass reliably
- Implementation and Launch
    - 🛑 All [release blocker bugs](https://github.com/ZcashFoundation/zebra/issues?q=is%3Aopen+is%3Aissue+milestone%3A%22First+Alpha+Release%22+label%3AC-bug) have been fixed
    - ✅ Users can access the documentation to deploy `zebrad` nodes
- User Experience
    - ✅ Build completes within 30 minutes in Zebra's CI
    - ⚠️ `zebrad` executes normally
        - ✅ `zebrad`'s default logging works reasonably well in a terminal
        - ✅ panics, error logs, and warning logs are rare on mainnet
        - ⚠️ known panics, errors and warnings have open tickets

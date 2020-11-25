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

### Go/No-Go Criteria

- `zebrad` Functionality
    - [ ] `zebrad` can sync to mainnet tip
        - [ ] under perfect network conditions (within 2 - 3 hours)
        - [ ] under reasonable network conditions (in under 5 hours)
        - [ ] under sub-optimal network conditions (?)
    - [ ] `zebrad` can keep up with the mainnet tip after the initial sync
        - [ ] under perfect network conditions
        - [ ] under reasonable network conditions
        - [ ] under sub-optimal network conditions
    - [ ] `zebrad` can validate proof of work
    - [ ] `zebrad` can validate the transaction merkle tree
    - [ ] `zebrad` can serve blocks to peers
- `zebrad` Performance
    - [ ] `zebrad` functionality works on platforms that meet its system requirements
- Testing
    - [ ] CI Passes
        - [ ] Unit tests pass reliably
        - [ ] Property tests pass reliably
        - [ ] Acceptance tests pass reliably
- Network Readiness
    - [ ] `zebrad` can interact with other nodes
    - [ ] We have a mechanism to deal with misbehaving `zebrad` nodes
- Implementation and Launch
    - [ ] All blocker bugs have been fixed
    - [ ] Release notes are available
    - [ ] Known issues are clearly documented
    - [ ] Users can access the documentation to deploy `zebrad` nodes
- User Experience
    - [ ] Build completes within 20 minutes in Zebra's CI
    - [ ] `zebrad` executes normally
        - [ ] without any of the "usual" errors
        - [ ] without any severe warnings
    - [ ] Users can access resources to troubleshoot `zebrad` malfunction
- Support
    - [ ] we have a clearly documented mechanism for users to report issues with `zebrad`
    - [ ] we have the ability to reproduce reported issues
    - [ ] `zebrad` reports sufficient information to allow us to troubleshoot reported issues

## Future Releases

### System Requirements

_TBD_

### Build Requirements

_TBD_

### Supported Platforms

_TBD_

### Go/No-Go Criteria

- `zebrad` Functionality
    - [ ] `zebrad` can sync to mainnet tip
        - [ ] under excellent network conditions (within 2 - 5 hours)
        - reasonable and sub-optimal network conditions are not yet supported
    - [ ] `zebrad` can stay within a few blocks of the mainnet tip after the initial sync
        - [ ] under excellent network conditions
        - reasonable and sub-optimal network conditions are not yet supported
    - [ ] `zebrad` can validate proof of work
    - [ ] `zebrad` can validate the transaction merkle tree
    - [ ] `zebrad` can serve blocks to peers
- `zebrad` Performance
    - [ ] `zebrad` functionality works on platforms that meet its system requirements
- Testing
    - [ ] CI Passes
        - [ ] Unit tests pass reliably
        - [ ] Property tests pass reliably
        - [ ] Acceptance tests pass reliably
- Implementation and Launch
    - [ ] All release blocker bugs have been fixed
    - [ ] Release notes are available
    - [ ] Known issues are clearly documented
    - [ ] Users can access the documentation to deploy `zebrad` nodes
- User Experience
    - [ ] Build completes within 20 minutes in Zebra's CI
    - [ ] `zebrad` executes normally
        - [ ] `zebrad`'s default logging works reasonably well in a terminal
        - [ ] panics, error logs, and warning logs are rare
        - [ ] known panics, errors and warnings have open tickets
    - [ ] Users can access resources to troubleshoot `zebrad` malfunction
- Support
    - [ ] we have a clearly documented mechanism for users to report issues with `zebrad`
    - [ ] we have the ability to reproduce reported issues
    - [ ] `zebrad` reports sufficient information to allow us to troubleshoot reported issues

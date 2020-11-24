# Go/No-Go Release Criteria

## Alpha Release

In the first alpha release, we want Zebra to participate in the network and replicate the Zcash chain state. We also want to validate proof of work and the transaction merkle tree, and serve blocks to peers.

### System Requirements

_TBD_

### Build Requirements

_TBD_

### Supported Platforms

_TBD_

### Go/No-Go Criteria

- Zebrad Functionality
    - [ ] zebrad can sync to mainnet tip
        - [ ] under perfect network conditions (within 2 - 3 hours)
        - [ ] under reasonable network conditions (in under 5 hours)
        - [ ] under sub-optimal network conditions (?)
    - [ ] zebrad can keep up with the mainnet tip after the initial sync
        - [ ] under perfect network conditions
        - [ ] under reasonable network conditions
        - [ ] under sub-optimal network conditions
    - [ ] zebrad can validate proof of work
    - [ ] zebrad can validate the transaction merkle tree
    - [ ] zebrad can serve blocks to peers
- Zebrad Performance
    - [ ] _TBD_
- Testing
    - [ ] CI Passes
        - [ ] Unit tests pass reliably
        - [ ] Property tests pass reliably
        - [ ] Acceptance tests pass reliably
- Network Readiness
    - [ ] zebrad can interact with other nodes
    - [ ] We have a mechanism to deal with misbehaving zebrad nodes
- Implementation and Launch
    - [ ] All blocker bugs have been fixed
    - [ ] Release notes are available
    - [ ] Known issues are clearly documented
    - [ ] Users can access the documentation to deploy zebrad nodes
- User Experience
    - [ ] Build completes in a reasonable amount of time _(?)_
    - [ ] zebrad executes normally
        - [ ] without any of the "usual" errors
        - [ ] without any severe warnings
    - [ ] Users can access resources to troubleshoot zebrad malfunction
- Support
    - [ ] we have a clearly documented mechanism for users to report issues with zebrad
    - [ ] we have the ability to reproduce reported issues
    - [ ] zebrad reports sufficient information to allow us to troubleshoot reported issues

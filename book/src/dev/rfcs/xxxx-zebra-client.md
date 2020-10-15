- Feature Name: (`zebra-client`)
- Start Date: (2020-10-14)
- Design PR: [ZcashFoundation/zebra#0000](https://github.com/ZcashFoundation/zebra/pull/1163)
- Zebra Issue: [ZcashFoundation/zebra#0000](https://github.com/ZcashFoundation/zebra/issues/0000)

# Summary
[summary]: #summary

The `zebra-client` crate handles *client functionality*. Client functionality
is defined as all functionality related to a particular user's private data,
in contrast to the other full node functionality which handles public chain
state.  This includes:

- note and key management;
- transaction generation;
- a client component for `zebrad` that handles block chain scanning, with
appropriate side-channel protections;
- an RPC endpoint for `zebrad` that allows access to the client component;
- a `zebracli` binary that wraps basic wallet functionality and RPC queries in a command-line interface.

Client functionality is restricted to transparent and Sapling shielded
transactions; Sprout shielded transactions are not supported. (Users should
migrate to Sapling).

# Motivation
[motivation]: #motivation

We want to allow users to efficiently and securely send and receive funds via
Zebra. One challenge unique to Zcash is block chain scanning: because
shielded transactions reveal no metadata about the sender or receiver, users
must scan the block chain for relevant transactions using *viewing keys*.
This means that unlike a transparent blockchain with public transactions, a
full node must have online access to viewing keys to scan the chain. This
creates the risk of a privacy leak, because the node should not reveal which
viewing keys it has access to.

Block chain scanning requires a mechanism that allows users to manage and
store key material. This mechanism should also provide basic wallet
functionality, so that users can send and receive funds without requiring
third-party software.

To protect user privacy, this and all secret-dependent functionality should
be strongly isolated from the rest of the node implementation. Care should be
taken to protect against side channels that could reveal information about
viewing keys. To make this isolation easier, all secret-dependent
functionality is provided only by the `zebra-client` crate.

# Definitions
[definitions]: #definitions

- **client functionality**: all functionality related to a particular user's
private data, in contrast to other full node functionality which handles
public chain state.

- **block chain scanning**: the process of scanning the block chain for
relevant transactions using a viewing key, as described in [§4.19][ps_scan]
of the protocol specification.

- **viewing key**: Sapling shielded addresses support *viewing keys*, which
represent the capability to decrypt transactions, as described in
[§3.1][ps_keys] and [§4.2.2][ps_sapk] of the protocol specification.

- **task**: In this document, *task* refers specifically to a [Tokio
task][tokio-task]. In brief, a task is a light weight, non-blocking unit of
execution (green thread), similar to a Goroutine or Erlang process. Tasks
execute independently and are scheduled co-operatively using explicit yield
points. Tasks are executed on the Tokio runtime, which can either be single-
or multi-threaded.

[ps_scan]: https://zips.z.cash/protocol/canopy.pdf#saplingscan
[ps_keys]: https://zips.z.cash/protocol/canopy.pdf#addressesandkeys
[ps_sapk]: https://zips.z.cash/protocol/canopy.pdf#saplingkeycomponents
[tokio-task]: https://docs.rs/tokio/0.2.22/tokio/task/index.html

# Guide-level explanation
[guide-level-explanation]: #guide-level-explanation

There are two main parts of this functionality. The first is a `Client`
component running as part of `zebrad`, and the second is a `zebracli`
command-line tool.

The `Client` component is responsible for blockchain scanning. It maintains
its own distinct `sled` database, which stores the viewing keys it uses to
scan as well as the results of scanning. When a new block is added to the
chain state, the `Client` component is notified asynchronously using a
channel. For each Sapling shielded transaction in the block, the component
attempts to perform trial decryption of that transaction's notes using each
registered viewing key, as described in [§4.19][ps_scan]. If successful,
decrypted notes are saved to the database.

The [`PING`/`REJECT`][pingreject] attack demonstrates the importance of
decoupling execution of normal node operations from secret-dependent
operations. Zebra's network stack already makes it immune to those particular
attacks, because each peer connection is executed in a different task.
However, to eliminate this entire class of vulnerability, we execute the
`Client` component in its own task, decoupled from the rest of the node
functionality. In fact, each viewing key's scanning is performed
independently, as described in more detail below, with an analysis of
potential side-channels.

[pingreject]: https://eprint.iacr.org/2020/220.pdf

The second part is the `zebracli` command-line tool, which provides basic
wallet functionality. This tool manages spending keys and addresses, and
communicates with the `Client` component in `zebrad` to provide basic wallet
functionality. Specifically, `zebracli` uses a distinct RPC endpoint to load
viewing keys into `zebrad` and to query the results of block chain scanning.
`zebracli` can then use the results of those queries to generate transactions
and submit them to the network using `zebrad`.

This design upholds the principle of least authority by separating key
material required for spending funds from the key material required for block
chain scanning. This allows compartmentalization. For instance, a user could
in principle run `zebrad` on a cloud VPS with only their viewing keys and
store their spending keys on a laptop, or a user could run `zebrad` on a
local machine and store their spending keys in a hardware wallet. Both of
these use cases would require some additional tooling support, but are
possible with this design.

# Reference-level explanation
[reference-level-explanation]: #reference-level-explanation


zebra-client
- client component
    - (keeps track of where it’s scanned to)
    - (zebra-client, in side zebrad, runs in its own separate task, in case it crashes, it’s not noticeable)
    - (the scanning executes independently (but in the same process) of the normal node operation)
- zebra-cli talks to this subcomponent which is in a running zebrad
    - (can use servo/bincode + servo/ipc-channel to communicate with zebrad)

## Task isolation in Tokio

- cooperative multitasking is fine, IF you cooperate
- lots of tasks



<!-- This is the technical portion of the RFC. Explain the design in sufficient detail that: -->

<!-- - Its interaction with other features is clear. -->
<!-- - It is reasonably clear how the feature would be implemented, tested, monitored, and maintained. -->
<!-- - Corner cases are dissected by example. -->

<!-- The section should return to the examples given in the previous section, and explain more fully how the detailed proposal makes those examples work. -->

## Module Structure

<!-- Describe the crate and modules that will implement the feature.-->

zebra-client ( currently and empty stub) zebra-cli (does not exist yet)
zebra-rfc? (exists as an empty stub, we way have zebra-cli communicate with
zebra-client inside zebrad via an RPC method any/or a private IPC layer)

## Test Plan

<!-- Explain how the feature will be tested, including: -->
<!-- * tests for consensus-critical functionality -->
<!-- * existing test vectors, if available -->
<!-- * Zcash blockchain block test vectors (specify the network upgrade, feature, or block height and network) -->
<!-- * property testing or fuzzing -->

<!-- The tests should cover: -->
<!-- * positive cases: make sure the feature accepts valid inputs -->
<!--   * using block test vectors for each network upgrade provides some coverage of valid inputs -->
<!-- * negative cases: make sure the feature rejects invalid inputs -->
<!--   * make sure there is a test case for each error condition in the code -->
<!--   * if there are lots of potential errors, prioritise: -->
<!--     * consensus-critical errors -->
<!--     * security-critical errors, and -->
<!--     * likely errors -->
<!-- * edge cases: make sure that boundary conditions are correctly handled -->

# Drawbacks
[drawbacks]: #drawbacks

<!-- Why should we *not* do this?-->

Supporting a wallet assumes risk.  Effort required to implement wallet functionality.

- need to responsibly handle secret key material;
- currently we only handle public data.

# Rationale and alternatives
[rationale-and-alternatives]: #rationale-and-alternatives

<!-- - What makes this design a good design? -->
<!-- - Is this design a good basis for later designs or implementations? -->
<!-- - What other designs have been considered and what is the rationale for not choosing them? -->

- why have a separate RPC endpoint?
    - extra endpoints are cheap
    - allows segmentation by capability
    - alternative is error-prone after-the-fact ACLs like Tor control port filters

- What is the impact of not doing this?
    - We can't send money with zebra alone.
    - rely on third party wallet software to send funds with zebra
        - we need to provide basic functionality within zebra's trust boundary, rather than forcing users to additionally trust 3p software
        - there are great 3p wallets, we want to integrate with them, just don't want to rely on them 

- light client protocol 
    - does not address this use case, has different trust model (private lookup, no scanning)
    - 

# Unresolved questions
[unresolved-questions]: #unresolved-questions

<!-- - What parts of the design do you expect to resolve through the RFC process before this gets merged? -->
<!-- - What parts of the design do you expect to resolve through the implementation of this feature before stabilization? -->
<!-- - What related issues do you consider out of scope for this RFC that could be addressed in the future independently of the solution that comes out of this RFC? -->

- wait to fill this in until doing the detailed writeup.

# Future possibilities
[future-possibilities]: #future-possibilities

- split `Client` component into subprocess
    - this helps somewhat but the benefit is reduced by memory safety
    - not meaningful without other isolation (need to restrict `zebrad` from accessing viewing keys on disk, etc)

- hardware wallet integration for `zebracli`
    - having `zebracli` allows us to do this
    - much higher security ROI than subprocess
    - very cool future feature

- authenticate queries for a particular viewing key by proving knowledge of
the viewing key (requires crypto). this could allow public access to the
client endpoint

<!-- Think about what the natural extension and evolution of your proposal would -->
<!-- be and how it would affect Zebra and Zcash as a whole. Try to use this -->
<!-- section as a tool to more fully consider all possible -->
<!-- interactions with the project and cryptocurrency ecosystem in your proposal. -->
<!-- Also consider how the this all fits into the roadmap for the project -->
<!-- and of the relevant sub-team. -->

<!-- This is also a good place to "dump ideas", if they are out of scope for the -->
<!-- RFC you are writing but otherwise related. -->

<!-- If you have tried and cannot think of any future possibilities, -->
<!-- you may simply state that you cannot think of anything. -->

<!-- Note that having something written down in the future-possibilities section -->
<!-- is not a reason to accept the current or a future RFC; such notes should be -->
<!-- in the section on motivation or rationale in this or subsequent RFCs. -->
<!-- The section merely provides additional information. -->

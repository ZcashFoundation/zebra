- Feature Name: (`zebra-client`)
- Start Date: (2020-10-14)
- Design PR: [ZcashFoundation/zebra#0000](https://github.com/ZcashFoundation/zebra/pull/1163)
- Zebra Issue: [ZcashFoundation/zebra#0000](https://github.com/ZcashFoundation/zebra/issues/0000)

> **Status: SUPERSEDED**
>
> This RFC proposed building wallet functionality directly into Zebra. After
> initial development, the `zebra-client` and `zebra-cli` crates were removed
> in May 2023 ([PR #6726](https://github.com/ZcashFoundation/zebra/pull/6726))
> in favor of external wallet tools.
>
> **Current Architecture:** Zebra now serves as a backend for external wallet
> tools through:
>
> - [Lightwalletd integration](../../user/lightwalletd.md) - handles blockchain
>   scanning and wallet synchronization
> - Standard RPC methods (`sendrawtransaction`, `getrawtransaction`, etc.)
> - External wallets like [Zecwallet](https://github.com/adityapk00/zecwallet-light-cli)
>
> This approach provides better separation of concerns: Zebra remains a pure
> full node with no user private data, while wallet functionality is handled
> by specialized external tools.
>
> This RFC is preserved for historical reference.

# Summary

[summary]: #summary

The `zebra-client` crate handles _client functionality_. Client functionality
is defined as all functionality related to a particular user's private data,
in contrast to the other full node functionality which handles public chain
state. This includes:

- note and key management;
- transaction generation;
- a client component for `zebrad` that handles block chain scanning, with
  appropriate side-channel protections;
- an RPC endpoint for `zebrad` that allows access to the client component;
- Rust library code that implements basic wallet functionality;
- a `zebra-cli` binary that wraps the wallet library and RPC queries in a command-line interface.

Client functionality is restricted to transparent and Sapling shielded
transactions; Sprout shielded transactions are not supported. (Users should
migrate to Sapling).

# Motivation

[motivation]: #motivation

We want to allow users to efficiently and securely send and receive funds via
Zebra. One challenge unique to Zcash is block chain scanning: because
shielded transactions reveal no metadata about the sender or receiver, users
must scan the block chain for relevant transactions using _viewing keys_.
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

- **viewing key**: Sapling shielded addresses support _viewing keys_, which
  represent the capability to decrypt transactions, as described in
  [§3.1][ps_keys] and [§4.2.2][ps_sapk] of the protocol specification.

- **task**: In this document, _task_ refers specifically to a [Tokio
  task][tokio-task]. In brief, a task is a light weight, non-blocking unit of
  execution (green thread), similar to a Goroutine or Erlang process. Tasks
  execute independently and are scheduled co-operatively using explicit yield
  points. Tasks are executed on the Tokio runtime, which can either be single-
  or multi-threaded.

[ps_scan]: https://zips.z.cash/protocol/protocol.pdf#saplingscan
[ps_keys]: https://zips.z.cash/protocol/protocol.pdf#addressesandkeys
[ps_sapk]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
[tokio-task]: https://docs.rs/tokio/0.2.22/tokio/task/index.html

# Guide-level explanation

[guide-level-explanation]: #guide-level-explanation

There are two main parts of this functionality. The first is a `Client`
component running as part of `zebrad`, and the second is a `zebra-cli`
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

The second part is the `zebra-cli` command-line tool, which provides basic
wallet functionality. This tool manages spending keys and addresses, and
communicates with the `Client` component in `zebrad` to provide basic wallet
functionality. Specifically, `zebra-cli` uses a distinct RPC endpoint to load
viewing keys into `zebrad` and to query the results of block chain scanning.
`zebra-cli` can then use the results of those queries to generate transactions
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

## State notifications

We want a way to subscribe to updates from the state system via a channel. For
the purposes of this RFC, these changes are in-flight, but in the future, these
could be used for a push-based RPC mechanism.

Subscribers can subscribe to all state change notifications as they come in.

Currently the `zebra_state::init()` method returns a `BoxService` that allows you to
make requests to the chain state. Instead, we would return a `(BoxService,
StateNotifications)` tuple, where `StateNotifications` is a new structure initially
defined as:

```rust
#[non_exhaustive]
pub struct StateNotifications {
  pub new_blocks: tokio::sync::watch::Receiver<Arc<Block>>,
}
```

Instead of making repeated polling requests to a state service to look for any
new blocks, this channel will push new blocks to a consumer as they come in,
for the consumer to use or discard at their discretion. This will be used by
the client component described below. This will also be needed for gossiping
blocks to other peers, as they are validated.

## Online client component

This component maintains its own Sled tree. See RFC#0005 for more details on Sled.

We use the following Sled trees:

| Tree                   | Keys                 | Values         |
| ---------------------- | -------------------- | -------------- |
| `viewing_keys`         | `IncomingViewingKey` | `String`       |
| `height_by_key`        | `IncomingViewingKey` | `BE32(height)` |
| `received_set_by_key`  | `IncomingViewingKey` | ?              |
| `spend_set_by_key`     | `IncomingViewingKey` | ?              |
| `nullifier_map_by_key` | `IncomingViewingKey` | ?              |

See <https://zips.z.cash/protocol/protocol.pdf#saplingscan>

Zcash structures are encoded using `ZcashSerialize`/`ZcashDeserialize`.

This component runs inside zebrad. After incoming viewing keys are registered,
it holds onto them in order to do blockchain scanning. The component keeps track
of where it’s scanned to (TODO: per key?). Runs in its own separate task, in
case it crashes, it’s not noticeable, and executes independently (but in the
same process) of the normal node operation.

In the case of the client component that needs to do blockchain scanning and
trial decryption, every valid block with non-coinbase transactions will need to
be checked and its transactions trial-decrypted with registered incoming viewing
keys to see if any notes have been received by the key's owner and if any notes
have already been spent elsewhere.

## RPC's

A specific set of _privileged_ RPC endpoints:

- Allows registering of incoming viewing keys with zebrad in order to do
  blockchain scanning
- Allows querying of the results of that scanning, to get wallet balance, etc
- Not authenticated to start (see 'Future possibilities')
- Users can control access by controlling access to the privileged endpoint (ie
  via a firewall)

Support for sending tx's via _non-privileged_ RPC endpoints, or via Stolon:

- sendTransaction: once you author a transaction you can gossip it via any
  Zcash node, not just a specific instance of zebrad

## Wallet functionality

- Holds on to your spending keys so you can author transactions
- Uses RPC methods to query the online client component inside zebrad about
  wallet balances

## CLI binary

- zebra-cli talks to the subcomponent running in zebrad
  - (can use servo/bincode to communicate with zebrad)
  - via the privileged (and possibly the unprivileged) RPC endpoints
  - can use [cap-std](https://blog.sunfishcode.online/introducing-cap-std/)
    to restrict filesystem and network access for zebra-client.
    See <https://github.com/ZcashFoundation/zebra/issues/2340>
  - can use the [tui crate](https://crates.io/crates/tui) to render a terminal UI

## Task isolation in Tokio

- TODO: fill in
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

Supporting a wallet assumes risk. Effort required to implement wallet functionality.

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

- What about the light client protocol?
  - does not address this use case, has different trust model (private lookup, no scanning)
  - we want our first client that interacts with zebrad to not have a long
    startup time, which a lightclient implementation would require
  - zebra-cli should be within the same trust and privacy boundary as the
    zebrad node it is interacting with
  - light client protocol as currently implemented requires stack assumptions
    such as protobufs and a hardcoded lightserver to talk to

- What about having one database per key?
  - easy to reliably delete or backup all data related to a single key
  - might use slightly more space/CPU
  - slightly harder to delete all the keys

# Unresolved questions

[unresolved-questions]: #unresolved-questions

<!-- - What parts of the design do you expect to resolve through the RFC process before this gets merged? -->
<!-- - What parts of the design do you expect to resolve through the implementation of this feature before stabilization? -->
<!-- - What related issues do you consider out of scope for this RFC that could be addressed in the future independently of the solution that comes out of this RFC? -->

- wait to fill this in until doing the detailed writeup.

# Future possibilities

[future-possibilities]: #future-possibilities

- [BlazeSync algorithm](https://forum.zcashcommunity.com/t/zecwallets-blazesync-sync-entire-chain-in-60s/39447)
  for fast syncing, like Zecwallet

- mandatory sweeps for legacy keys
  - blazingly fast wallet startup, to match `zebrad`'s blazingly fast sync
  - generate unified address from a new seed phrase (or one provided by the user)
  - user can just backup seed phrase rather than a set of private keys
  - handles arbitrary keys from `zcashd` and other wallets, even if they weren't generated from a seed phrase
  - ~handles Sprout funds without `zebra-client` having to support Sprout balances~
  - startup is incredibly fast
    - sweep takes a few minutes to be confirmed
    - scanning the entire chain could take hours
    - if we know when the seed phrase was created, we can skip millions of blocks during scanning
  - sweeps can also be initiated by the user for non-linkability / performance / refresh
  - sweeps should handle the "block reward recipient" case where there are a lot of small outputs
  - initial release could support mandatory sweeps, and future releases could support legacy keys

- split `Client` component into subprocess - this helps somewhat but the benefit is reduced by our preexisting memory safety, thanks to Rust - not meaningful without other isolation (need to restrict `zebrad` from accessing viewing keys on disk, etc) - could use [cap-std](https://blog.sunfishcode.online/introducing-cap-std/)
  to restrict filesystem and network access for zebra-client.
  See <https://github.com/ZcashFoundation/zebra/issues/2340> - instead of process isolation, maybe you actually want the Light Client
  Protocol, or something similar?

- hardware wallet integration for `zebra-cli`
  - having `zebra-cli` allows us to do this
  - much higher security ROI than subprocess
  - very cool future feature

- authenticate queries for a particular viewing key by proving knowledge of the
  viewing key (requires crypto). this could allow public access to the client
  endpoint

- Use Unified Addresses only, no legacy addrs.

<!-- Think about what the natural extension and evolution of your proposal would -->
<!-- be and how it would affect Zebra and Zcash as a whole. Try to use this -->
<!-- section as a tool to more fully consider all possible -->
<!-- interactions with the project and cryptocurrency ecosystem in your proposal. -->
<!-- Also consider how this all fits into the roadmap for the project -->
<!-- and of the relevant sub-team. -->

<!-- This is also a good place to "dump ideas", if they are out of scope for the -->
<!-- RFC you are writing but otherwise related. -->

<!-- If you have tried and cannot think of any future possibilities, -->
<!-- you may simply state that you cannot think of anything. -->

<!-- Note that having something written down in the future-possibilities section -->
<!-- is not a reason to accept the current or a future RFC; such notes should be -->
<!-- in the section on motivation or rationale in this or subsequent RFCs. -->
<!-- The section merely provides additional information. -->

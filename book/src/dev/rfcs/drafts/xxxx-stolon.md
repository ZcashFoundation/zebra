- Feature Name: `stolon`
- Start Date: 2020-08-31
- Design PR: [ZcashFoundation/zebra#1006](https://github.com/ZcashFoundation/zebra/pull/1006)
- Zebra Issue: [ZcashFoundation/zebra#1643](https://github.com/ZcashFoundation/zebra/issues/1643)
- Zebra Implementation: [ZcashFoundation/zebra#1007](https://github.com/ZcashFoundation/zebra/pull/1007)

# Draft

Note: This is a draft Zebra RFC. See
[ZcashFoundation/zebra#1643](https://github.com/ZcashFoundation/zebra/issues/1643)
for more details.

# Summary

[summary]: #summary

Stolon is a transaction diffusion algorithm inspired by Dandelion, with
additional privacy properties. Dandelion diffuses transactions across a p2p
network in two phases, a "stem" phase where transactions are relayed in a line
and a "fluff" phase where transactions are broadcast as widely as possible.
[Stolon][wiki], named for runners that extend horizontally at or just below the
soil to spread new plants, tweaks this approach to perform the "stem" phase
over ephemeral Tor connections and the "fluff" phase over the clearnet p2p
network.

Stolon is designed to for use both by full nodes such as Zebra and Zcashd as
well as by wallet software. This RFC describes:

- the threat model Stolon addresses;
- how Zebra's architecture makes Stolon easy to implement;
- how Stolon can be used by Zebra or Zcashd;
- and how Stolon can be used by wallet software.

[wiki]: https://en.wikipedia.org/wiki/Stolon

# Motivation

[motivation]: #motivation

At a high level, Zcash can be thought of as a fault-tolerant system for
replicating ledger state (such as transactions and blocks) among the
participants in the Zcash network. This ledger state is public and
globally-shared, as in Bitcoin, but unlike Bitcoin, Zcash uses advanced
cryptographic techniques to provide a private ledger using public data.

This functionality is provided by shielded transactions, whose public data
(sent over the network and stored in the block chain) does not reveal private
information about ledger state (such as transaction amounts, senders, or
receivers).

However, shielded transactions are created by some particular user, and this
poses a linkability challenge: how can we manage the transition of some
particular transaction from its creation by some particular user into being
part of the public, globally-shared chain state, _without revealing its
provenance_. This is the problem of private transaction diffusion.

Zcashd's transaction diffusion mechanism does not attempt to provide privacy.
Zebra does not yet have any transaction diffusion mechanism, so designing one
for Zebra is an opportunity to improve on this state of affairs. This RFC
proposes Stolon.

As noted in the [definitions] section, this RFC considers a broad definition of
transaction diffusion that encompasses the complete process from the initial
production of a transaction up to its inclusion in a mined block. Thus, while
Stolon is primarily designed as a transaction diffusion mechanism for Zebra, it
is also intended to be useful for wallet software.

This problem is made more important to address by the existence of Zebra.
Unlike Zcashd, Zebra uses its scalable network architecture to attempt to
connect to as many peers as possible, so every Zebra node is a supernode in the
connection graph. And because Zebra is designed to support observability and
instrumentation, Zebra can be trivially repurposed as the network monitoring
adversary described in the Dandelion paper by setting an appropriate tracing
filter.

# Definitions

[definitions]: #definitions

- **transaction diffusion**: the complete process from the initial production
  of a transaction up to its inclusion in a mined block.

- **relay phase**: the initial, small-fanout diffusion phase, described
  [below][guide-level-explanation].

- **broadcast phase**: the final, wide-fanout diffusion phase, described
  [below][guide-level-explanation].

- **node**: a participant in the Zcash network.

- **wallet software**: software that generates transactions. This can either
  be standalone software (light wallets) or integrated with a full node.

# Guide-level explanation

[guide-level-explanation]: #guide-level-explanation

[Stolon][wiki], named for runners that extend horizontally at or just below
the soil to spread new plants, diffuses transactions in two phases:

[wiki]: https://en.wikipedia.org/wiki/Stolon

- a small-fanout relay phase, performed over special, ephemeral Tor
  connections. These connections are isolated from all other node state and
  from each other, and use a special, minimally-distinguishable Zcash
  handshake.
- a wide-fanout broadcast phase, performed over clearnet p2p connections,
  spreading transactions as quickly as possible.

Transaction diffusion starts in the relay phase, and transitions to the
broadcast phase with random probability.

The relay phase does not require any changes to the Zcash network protocol,
but it does require a protocol implementation that can create completely
isolated network connections and configure them not to leak distinguishing
information. This is impossible with Zcashd's implementation, which
commingles protocol state between all connections, but is easy to implement
using `zebra-network`. The implementation of the relay phase is provided by a
new `stolon` crate, which also handles communication with Tor's SOCKS proxy.

This crate can be used as a Rust library, and provides a standalone `stolon`
binary that reads a transaction from stdin and relays it to the Zcash
network. This allows the relay phase to be easily performed by wallet
software or Zcashd, by integrating the `stolon` binary or library.

# Reference-level explanation

[reference-level-explanation]: #reference-level-explanation

## Dandelion

Stolon is inspired by Dandelion, a design for more private transaction
diffusion in Bitcoin. Dandelion is described in detail in [the Dandelion
paper][d], [the Dandelion++ paper][dpp], and in [BIP156].

At a high level, Dandelion works as follows:

- Each node chooses two "Dandelion destinations" among its peer set, which it
  will use for transaction relaying. These destination peers serve a similar
  role to [guard nodes in Tor][tor-guard], and each node's choice of
  destination peers forms a subgraph of the full connection graph, called the
  "privacy subgraph".

- In the "stem phase", transactions are forwarded along the privacy subgraph.
  This forwarding is performed with Dandelion-specific network messages and
  transactions are stored in a special "stempool" separate from the ordinary
  mempool. At each step, a transaction can transition to the fluff phase with
  small probability.

- In the "fluff phase", transactions are broadcast as widely as possible across
  the full connection graph.

An adversary maintaining connections to many peers and monitoring which
transactions they advertise has much more limited insight into the privacy
subgraph than the full connection graph. Forwarding transactions along the
privacy subgraph aims to obscure the origin of any particular transaction.

The original Dandelion design in [BIP156] required changes to the Bitcoin
network protocol, and was not adopted. A simplified version known as
"Dandelion-lite" also exists; it drops the special network messages and only
performs the stem phase for newly-generated transactions.

[tor-guard]: https://petsymposium.org/2014/papers/Dingledine.pdf
[d]: https://arxiv.org/pdf/1701.04439.pdf
[dpp]: https://arxiv.org/pdf/1805.11060.pdf
[BIP156]: https://github.com/bitcoin/bips/blob/master/bip-0156.mediawiki

Dandelion's privacy gains are estimated empirically, but it's unclear how
well these estimates apply to Zcash in practice. For instance, the original
paper studies information flow in 16-regular graphs, on the basis that the
default Bitcoin configuration creates 8 peer connections. But in practice the
connection graph is not regular, because outbound connections are not
uniformly distributed across other nodes. This issue becomes more extreme in
the case of Zcash, where every Zebra node is a supernode, so further analysis
of information flow in irregular graph models would be desirable.

## Transaction messages in Zcash

Zcash inherits the Bitcoin networking protocol. Transactions are handled
using the following messages:

- `inv`: the `inv` message contains a list of object hashes, and is used by a
  node to advertise the availability of some object to other peers. In
  particular, transactions can be advertised to other peers by sending them an
  `inv` message containing the hash of the transaction.

- `getdata`: the `getdata` message requests objects by hash, and is used by a
  node to request the contents of some previously-advertised data.

- `tx`: the `tx` message contains transaction data. It can be sent in response
  to a `getdata` or unsolicited.

- `mempool`: the `mempool` message requests an `inv` message containing a list
  of hashes of transactions in the node's mempool.

## Stolon

As described [above][guide-level-explanation], Stolon has two phases: a relay
phase, using isolated, minimally-distinguishable Tor connections, and a
broadcast phase, using clearnet p2p connections. Stolon has the following
parameters:

- The _transition probability_ `0 < q < 1`;
- The _fanout degree_ `f`.

Newly created transactions begin in the relay phase. When a node receives an
unsolicited `tx` message, it assumes that the transaction it contains is in the
relay phase. It attempts to commit the transaction to its mempool. If
successful, the node continues relaying with probability `1-q` or enters the
broadcast phase with probability `q`.

To continue relaying, the node chooses `f` relay peers uniformly at random from
its active peer connections. For each relay peer, the node attempts to open an
ephemeral Tor connection to that peer (as described in more detail below),
forwards the transaction in an unsolicited `tx` message, and closes the
connection. Failures are ignored.

To enter the broadcast phase, the node immediately sends an `inv` message
containing the transaction hash to every connected peer.

When a node receives an `inv` message advertising a transaction, it assumes
that the advertised transaction is in the broadcast phase. If the hash is
unknown, it attempts to download the transaction and commit the transaction to
its mempool. If successful, it broadcasts the transaction to its connected
peers.

Because transactions enter the mempool in the relay phase, it would be possible
for an adversary to learn of the presence of a new transaction in advance of it
entering the broadcast phase. To prevent this, the mempool can maintain a list
of relay transaction hashes used to filter responses to mempool queries. Hashes
can be removed from the relay transaction list when they are observed from
other peers, for instance by removing a hash from the relay transaction list as
part of a mempool membership query (since the mempool is queried to determine
whether an advertised transaction is already known).

## Isolated, minimally-distinguishable connections

Using Zcash or Bitcoin over Tor is generally inadvisable. The reason is that
while Tor provides network-level anonymity, the Bitcoin p2p protocol used in
Zcash leaks an enormous amount of distinguishing information about a node's
state, with at best half-hearted and ineffective fingerprinting
countermeasures.

For instance, peer discovery is implemented the `getaddr` message, which
requests an `addr` message containing a list of peers and their last-seen
timestamps. The last-seen timestamps record the time the responding node last
received a message from that peer. In addition to node fingerprinting, this is
probably a useful additional source of information for an adversary seeking to
trace transaction origins. Zcashd attempts to prevent fingerprinting by
serving at most one `addr` message per peer connection, but does so
statelessly, so tearing down and rebuilding the connection defeats this
prevention.

Building Zcash connections over Tor is therefore only useful if the connections
can be:

- completely isolated from all other node state, so that peers cannot make
  queries revealing information about the node;

- constructed with minimal distinguishing information, such as IP addresses,
  timestamps, etc.

This is not possible with the network implementation in Zcashd but is fairly
straightforward to implement using `zebra-network`. The required steps are:

1. Change the `Handshake` service to allow full configuration of the data sent
    while constructing a connection;

2. Provide a new method that constructs an isolated connection:

    ```rust
    pub async fn connect_isolated(
        conn: TcpStream,
        user_agent: String,
    ) -> Result<BoxService<Request, Response, BoxedStdError>, BoxedStdError>
    ```

    This method takes an existing `TcpStream` so that it can be used with a
    pre-constructed Tor circuit. This method should construct a new `Handshake`
    instance and configure it with the fixed default values specified below.
    The `Handshake` service takes a handler `Service` used to process inbound
    requests from the remote peer; `connect_isolated` should pass a no-op
    `service_fn` that ignores requests.

3. Use `tokio-socks` to construct a `TcpStream` backed by a fresh Tor circuit,
    and pass it to `connect_isolated`.

This allows Zebra, or any other user of `zebra-network`, to construct anonymous
connections to Zcash peers. However, there is some amount of work required to
glue these pieces together.

This glue is implemented in the `stolon` crate, which provides a single method
for the relay operation:

```rust
pub async fn send_transaction(
    tor_addr: SocketAddr,
    peer_addr: SocketAddr,
    transaction: Arc<Transaction>,
) -> Result<(), BoxError>
```

This method requires knowledge of the peer address and the use of an async Rust
library. However, this may be inconvenient for wallet software. To address this
use-case, the `stolon` crate provides a standalone `stolon` binary, which reads
a Zcash transaction from stdin, obtains peers by performing a DNS-over-Tor
lookup to the Zcash Foundation's DNS seeders, and then sends the transaction to
those peers.

Zcashd can implement Stolon by using the `stolon` crate as a library.

### Handshake parameters

To be minimally-distinguishable, the version message sent during the handshake
should have the following fields:

| Field          | Value                                                 |
| -------------- | ----------------------------------------------------- |
| `version`      | Current supported Zebra network version               |
| `services`     | `0`, no advertised services                           |
| `timestamp`    | Local UNIX timestamp, truncated to 5-minute intervals |
| `address_recv` | (`NODE_ADDR`, `0.0.0.0:8233`)                         |
| `address_from` | (`0`, `0.0.0.0:8233`)                                 |
| `nonce`        | Random value                                          |
| `user_agent`   | `""`, the empty string                                |
| `start_height` | `0`                                                   |
| `relay`        | `false`                                               |

The timestamp is chosen to be truncated to 5-minute intervals as a balance
between two goals:

- providing minimally-distinguishable data in the handshake;
- not interfering with `zcashd`'s existing code.

There is no specification of the network protocol, so when considering the
latter goal the best we can do is to make inferences based on the current state
of the `zcashd` source code. Currently, `zcashd` only uses the handshake
timestamp at the end of parsing the version message:

```cpp
pfrom->nTimeOffset = timeWarning.AddTimeData(pfrom->addr, nTime, GetTime());
```

The `AddTimeData` method defined in `src/timedata.cpp` is a no-op as long as
the difference between the specified timestamp and `zcashd`'s local time is less
than a `TIMEDATA_WARNING_THRESHOLD` set to 10 minutes. Otherwise, the data is fed
into a process that attempts to detect when the local time is wrong.
Truncating to the nearest 5 minutes (half of `TIMEDATA_WARNING_THRESHOLD`)
attempts to try to stay within this interval, so as to not confuse `zcashd`.

In general, truncation may not be sufficient to prevent an observer from
obtaining distinguishing information, because of aliasing between the truncated
data and the original signal. For instance, continuously broadcasting the
node's timestamp truncated to a 5-minute interval would not hide the node's
clock skew, which would be revealed at the moment the node switched to the next
truncation. However, this is unlikely to be a problem in practice, because the
number of samples an observer can obtain from a single node is likely to be
small. And, unlike other approaches like adding random noise, truncation is
simple, reliable, and deterministic, making it easy to test.

## Deployment considerations and non-Tor relay fallback

Unfortunately, Tor is difficult to use as a library and its ease of deployment
varies by platform. We therefore need to consider how to support Tor
integrations and to provide a fallback mechanism when Tor is not available.

To handle fallback, a Zebra node without Tor access can perform the relay
phase over an existing peer connection, rather than over an ephemeral Tor
connection. This is much worse for privacy, but is similar to the design of
Dandelion-lite. Ideally, Zebra+Tor is the common case for Zcash nodes, and
Zebra nodes with Tor support can improve network privacy for Zebra without
Tor and Zcashd nodes.

We should consider how we can encourage Tor deployments for Zebra. One option
would be to recommend Docker as the primary install mechanism, and bundle a Tor
relay image with the Zebra image. Another would be to attempt to package Zebra
with a dependency on Tor.

# Drawbacks

[drawbacks]: #drawbacks

- This design is more complicated than simply immediately broadcasting
  transactions to all peers.

- This design requires integration with external software (Tor) to be
  effective, although the fallback method probably provides better privacy than
  immediate broadcast.

- Zcashd keeps IP-based peer reputation scores and may reject connections from
  Tor exits if too many invalid transactions are sent from one exit. However,
  only one relay-to-Zcashd has to succeed for Zcashd to broadcast a transaction
  to all of its peers, and Zebra, which does not yet have a reputation system,
  can implement one that's aware of transaction relaying. It's unclear whether
  this will be a problem in practice, and the solution would probably be to fix
  Zcashd.

# Rationale and alternatives

[rationale-and-alternatives]: #rationale-and-alternatives

- Dandelion without Tor: this option has the advantage of not requiring
  integration with external software, but it provides weaker privacy benefits.
  Unlike Dandelion, in Stolon, a single step in the relay phase completely
  unlinks a transaction from its source.

- Overlap with wallet code: the code required to implement Stolon is reusable
  for wallet software, and provides strong anonymity properties for that use
  case regardless of how widely Zebra is deployed.

- It's something: Zebra needs a transaction diffusion mechanism, and this one
  can share code with wallet software and provide strong benefits in the
  Zebra+Tor case, even if the privacy properties of the fallback mechanism are
  unknown.

- Using Tor connections only for wallets, not for node relay: this doesn't save
  much complexity, since the Tor code has to exist anyways, but it does prevent
  Zebra+Tor nodes from anonymously relaying transactions on behalf of software
  that does not use Tor connections.

- Guard peers: instead of using random clearnet peers for non-Tor fallback, we
  could choose `f` guard peers as in Tor. This is probably workable but would
  complicate the backpressure design of the peer set somewhat, as we would
  probably have to dedicate `f` peers to _only_ serve transaction relay
  requests. This might introduce additional network distinguishers.

- Mixnets: Tor provides strong privacy properties in practice, and can be used
  today. In contrast, mixnets are [not yet][mixnet] at the same level of
  technology readiness and are significantly more complex to integrate.

[mixnet]: https://web.archive.org/web/20200422224714/https://www.zfnd.org/blog/mixnet-production-readiness/

# Prior art

[prior-art]: #prior-art

This design is inspired by Dandelion and its variants, as described above.

Zcashd's transaction diffusion algorithm immediately advertises new
transactions to all peers, and does not attempt to hide their source.

# Unresolved questions

[unresolved-questions]: #unresolved-questions

- How should we encourage Zebra to be deployed alongside Tor?

- What privacy benefits, if any, are provided by the clearnet fallback relay
  method, compared to Zcashd's baseline?

- What parameter choices should we make? `q = 0.33`, `f = 2` seems like a
  reasonable starting point, but we should use actual estimates, not just made
  up parameters.

- How can we help wallet software integrate Stolon?

# Future possibilities

[future-possibilities]: #future-possibilities

- Quantitative analysis of Stolon's privacy properties in the fallback relay
  case.

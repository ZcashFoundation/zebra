# A New Network Stack for Zcash

In our [2020 Engineering Roadmap][roadmap], we gave an overview of our plans
for Zebra.  Announced last summer at Zcon1, Zebra aims to support the core
strength of Zcash – its best-in-class cryptography – by placing it on a solid
foundation, providing a modern, modular implementation that can be broken into
components and used in many different contexts.  In that post, we briefly
described the new network stack we designed and implemented for Zebra.  As a
fork of Bitcoin, Zcash inherited Bitcoin's network protocol; in this post,
we'll do a deep dive on Zebra's network stack.

The new design is based on service-oriented architecture concepts from the
[tower library][tower], used in Buoyant's [linkerd][linkerd].  It isolates the
Bitcoin state machine for each peer connection, exposing only a clean
request/response API, and then encapsulates all the peer connections behind a
connection pool that can load-balance outbound requests over all available
peers.  The connection pool is dynamically sized in response to
[backpressure][backpressure-tokio], automatically crawling the network to find
new peers when outbound demand is high, and closing existing connections to
shed load when inbound demand is high.

## Bitcoin's Legacy Network Protocol

Zcash was originally a fork of Bitcoin, adding fully private transactions
implemented using zero-knowledge proofs.  As the first ever production-scale
deployment of [zk-SNARKs][snark], it's understandable that its original
development was focused on bringing zk-SNARKs to production, rather than
redesigning the Bitcoin blockchain.  But this meant that Zcash inherited its
network protocol from Bitcoin, which in turn inherited it from a
poorly-specified C++ codebase written in 2009 by Satoshi before their
disappearance.

The Bitcoin network protocol does not have any formalized concept of requests
or responses.  Instead, nodes send each other messages, which are processed one
at a time and might or might not cause the recipient to generate other
messages.  Often, those messages can also be sent unsolicited.  For instance,
node `A` might send a `getblocks` message to node `B`, and node `B` might
“respond” with an `inv` message advertising inventory to node `A`, but `B`’s
`inv` message is not connected in any way to `A`’s `getblocks` message.  Since
`B` can also send `A` unsolicited `inv` messages as part of the gossip
protocol, both nodes need to maintain complex connection state to understand
each other.

In `zcashd`, this is performed by a [900-line function in
`main.cpp`][zcashd-process], and in `bitcoind`, which has been refactored since
`zcashd` was forked, it’s performed by [this 1400-line C++
function][bitcoin-process].  Not only is the required connection state
enormous, making it very difficult to exhaustively understand and test, it's
also shared between different peer connections.

When thinking about what we wanted our network layer to look like, we knew this
was what we didn’t want.  An enormous, complex state machine shared between
connections is a sure sign of future trouble for maintainability, security, and
performance.  So what would be the appropriate foundation?

## A `tower`ing Interlude

## A Request/Response Protocol for Zcash

## Per-Peer Protocol Translation

## Building a Connection Pool

## Crawling the Network


[2020]: https://www.zfnd.org/blog/eng-roadmap-2020/
[tower]: https://docs.rs/tower
[linkerd]: https://linkerd.io
[backpressure-tokio]: https://tokio.rs/docs/overview/#backpressure
[snark]: https://z.cash/technology/zksnarks/
[bitcoin-process]: https://github.com/bitcoin/bitcoin/blob/c7e6b3b343e836ff41e9a8872187e0b24f13064d/src/net_processing.cpp#L1883-L3220
[zcashd-process]: https://github.com/zcash/zcash/blob/f0003239f87c2bfcff18986144e080c7ed501eb1/src/main.cpp#L5404-L6310

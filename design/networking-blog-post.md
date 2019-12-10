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

## A `tower`ing Interlude

## A Request/Response Protocol for Zcash

## Per-Peer Protocol Translation

## Building a Connection Pool

## Crawling the Network

[2020]: https://www.zfnd.org/blog/eng-roadmap-2020/
[tower]: https://docs.rs/tower
[linkerd]: https://linkerd.io
[backpressure-tokio]: https://tokio.rs/docs/overview/#backpressure

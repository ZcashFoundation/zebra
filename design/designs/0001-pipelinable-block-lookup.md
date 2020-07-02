- Feature Name: Pipelinable Block Syncing and Lookup
- Start Date: 2020-07-02
- Design PR: [rust-lang/rfcs#0000](https://github.com/rust-lang/rfcs/pull/0000)
- Rust Issue: [rust-lang/rust#0000](https://github.com/rust-lang/rust/issues/0000)

# Summary
[summary]: #summary



# Motivation
[motivation]: #motivation

To sync the chain, we need to find out which blocks to download and then download them.  Downloaded blocks can then be fed into the verification system and (assuming they verify correctly) into the state system.  In `zcashd`, blocks are processed one at a time.  In Zebra, however, we want to be able to pipeline block download and verification operations, using futures to explicitly specify logical dependencies between sub-tasks, which we execute concurrently and potentially out-of-order on a threadpool. This means that the procedure we use to determine which blocks to download must look somewhat different than `zcashd`.

# Guide-level explanation
[guide-level-explanation]: #guide-level-explanation

Explain the proposal as if it was already included in the language and you were teaching it to another Rust programmer. That generally means:

- Introducing new named concepts.
- Explaining the feature largely in terms of examples.
- Explaining how Rust programmers should *think* about the feature, and how it should impact the way they use Rust. It should explain the impact as concretely as possible.
- If applicable, provide sample error messages, deprecation warnings, or migration guidance.
- If applicable, describe the differences between teaching this to existing Rust programmers and new Rust programmers.

For implementation-oriented RFCs (e.g. for compiler internals), this section should focus on how compiler contributors should think about the change, and give examples of its concrete impact. For policy RFCs, this section should provide an example-driven introduction to the policy, and explain its impact in concrete terms.

# Reference-level explanation
[reference-level-explanation]: #reference-level-explanation

This is the technical portion of the RFC. Explain the design in sufficient detail that:

- Its interaction with other features is clear.
- It is reasonably clear how the feature would be implemented.
- Corner cases are dissected by example.

The section should return to the examples given in the previous section, and explain more fully how the detailed proposal makes those examples work.

# Drawbacks
[drawbacks]: #drawbacks

Why should we *not* do this?

# Rationale and alternatives
[rationale-and-alternatives]: #rationale-and-alternatives

- Why is this design the best in the space of possible designs?
- What other designs have been considered and what is the rationale for not choosing them?
- What is the impact of not doing this?

# Prior art
[prior-art]: #prior-art

Discuss prior art, both the good and the bad, in relation to this proposal.
A few examples of what this can include are:

- For language, library, cargo, tools, and compiler proposals: Does this feature exist in other programming languages and what experience have their community had?
- For community proposals: Is this done by some other community and what were their experiences with it?
- For other teams: What lessons can we learn from what other communities have done here?
- Papers: Are there any published papers or great posts that discuss this? If you have some relevant papers to refer to, this can serve as a more detailed theoretical background.

This section is intended to encourage you as an author to think about the lessons from other languages, provide readers of your RFC with a fuller picture.
If there is no prior art, that is fine - your ideas are interesting to us whether they are brand new or if it is an adaptation from other languages.

Note that while precedent set by other languages is some motivation, it does not on its own motivate an RFC.
Please also take into consideration that rust sometimes intentionally diverges from common language features.

# Unresolved questions
[unresolved-questions]: #unresolved-questions

- What parts of the design do you expect to resolve through the RFC process before this gets merged?
- What parts of the design do you expect to resolve through the implementation of this feature before stabilization?
- What related issues do you consider out of scope for this RFC that could be addressed in the future independently of the solution that comes out of this RFC?

# Future possibilities
[future-possibilities]: #future-possibilities

Think about what the natural extension and evolution of your proposal would
be and how it would affect the language and project as a whole in a holistic
way. Try to use this section as a tool to more fully consider all possible
interactions with the project and language in your proposal.
Also consider how the this all fits into the roadmap for the project
and of the relevant sub-team.

This is also a good place to "dump ideas", if they are out of scope for the
RFC you are writing but otherwise related.

If you have tried and cannot think of any future possibilities,
you may simply state that you cannot think of anything.

Note that having something written down in the future-possibilities section
is not a reason to accept the current or a future RFC; such notes should be
in the section on motivation or rationale in this or subsequent RFCs.
The section merely provides additional information.


# TODO convert this summary into the template

## Block fetching in Bitcoin

Zcash inherits its network protocol from Bitcoin. Bitcoin block fetching works roughly as follows.  A node can request block information from peers using either a `getblocks` or `getheaders` message.  Both of these messages contain a *block locator object* consisting of a sequence of block hashes.  The block hashes are ordered from highest to lowest, and represent checkpoints along the path from the node's current tip back to genesis.  The remote peer computes the intersection between its chain and the node's chain by scanning through the block locator for the first hash in its chain.  Then, it sends (up to) 500 subsequent block hashes in an `inv` message (in the case of `getblocks`) or (up to) 2000 block headers in a `headers` message (in the case of `getheaders`).  Note: `zcashd` reduces the `getheaders` count to 160, because Zcash headers are much larger than Bitcoin headers, as noted below.

The `headers` message sent after `getheaders` contains the actual block headers, while the `inv` message sent after `getblocks` contains only hashes, which have to be fetched with a `getdata` message.  In Bitcoin, the block headers are small relative to the size of the full block, but this is not always the case for Zcash, where the block headers are much larger due to the use of Equihash and many blocks have only a few transactions.  Also, `getblocks` allows parallelizing block downloads, while `getheaders` doesn't. For these reasons and because we know we need full blocks anyways, we should probably use `getblocks`.

The `getblocks` Bitcoin message corresponds to our `zebra_network::Request::FindBlocksByHash`, and the `getdata` message is generated by `zebra_network::Request::Blocks`.

## Pipelining block verification

As mentioned above, our goal is to be able to pipeline block download and verification.  This means that the process for block lookup should ideally attempt to fetch and begin verification of future blocks without blocking on complete verification of all earlier blocks.  To do this, we split the chain state into the *verified* block chain (held by the state component) and the *prospective* block chain (held only by the syncer), and use the following algorithm to pursue prospective chain tips.

#### ObtainTips

1. Query the current state to construct the sequence of hashes
```
[tip, tip-1, tip-2, ..., tip-9, tip-20, tip-40, tip-80, tip-160 ]
```
The precise construction is unimportant, but this should have a Bitcoin-style dense-first, then-sparse hash structure.

The initial state should contain the genesis block for the relevant network. So the sequence of hashes will only contain the genesis block
```
[genesis ]
```
The network will respond with a list of hashes, starting at the child of the genesis block.

2. Make a `FindBlocksByHash` request to the network `F` times, where `F` is a fanout parameter, to get `resp1, ..., respF`.

3. For each response, starting from the beginning of the list, prune any block hashes already included in the state, stopping at the first unknown hash to get `resp1', ..., respF'`.  (These lists may be empty).

4. Combine the last elements of each list into a set; this is the set of prospective tips.

5. Combine all elements of each list into a set, and queue download and verification of those blocks.

6. If there are any prospective tips, call ExtendTips, which returns a new set of prospective tips. Continue calling ExtendTips with this new set, until there are no more prospective tips.

7. Restart after some delay, say 15 seconds.

#### ExtendTips

1. Remove all prospective tips from the set of prospective tips, then iterate through them.  For each removed tip:

2. Create a `FindBlocksByHash` request consisting of just the prospective tip.  Send this request to the network `F` times.

3. For each response, check whether the first hash in the response is a genesis block (for either the main or test network). If so, discard the response. It indicates that the remote peer does not have any blocks following the prospective tip. (Or that the remote peer is on the wrong network.)

4. Combine the last elements of the remaining responses into a set, and add this set to the set of prospective tips.

5. Combine all elements of the remaining responses into a set, and queue download and verification of those blocks.

### DoS resistance

Because this strategy aggressively downloads any available blocks, it could be vulnerable to a DoS attack, where a malicious peer feeds us bogus chain tips, causing us to waste network and CPU on blocks that will never be valid.  However, because we separate block finding from block downloading, and because of the design of our network stack, this attack is probably not feasible.  The primary reason is that `zebra_network` randomly loadbalances outbound requests over all available peers.

Consider a malicious peer who responds to block discovery with a bogus list of hashes.  We will eagerly attempt to download all of those bogus blocks, but our requests to do so will be randomly load-balanced to other peers, who are unlikely to know about the bogus blocks. When we try to extend a bogus tip, the extension request will also be randomly load-balanced, so it will likely be routed to a peer that doesn't know about it and can't extend it. And because we perform multiple block discovery queries, which will also be randomly load balanced, we're unlikely to get stuck on a false chain tip.

### Fork-finding

When starting from a verified chain tip, the choice of block locator can find forks at least up to the reorg limit (99 blocks). When extending a prospective tip, forks are ignored, but this is fine, since unless we are prefetching the longest chain, we won't be able to keep extending the tip prospectively.

### Retries and Fanout

We should consider the fanout parameter `F` and the retry policy for the different requests.  I'm not sure whether we need to retry requests to discover new block hashes, since the fanout may already provide redundancy.  For the block requests themselves, we should have a retry policy with a limited number of attempts, enough to insulate against network failures but not so many that we would retry a bogus block indefinitely. Maybe fanout 4 and 3 retries?

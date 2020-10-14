- Feature Name: (`zebra-client`)
- Start Date: (fill me in with today's date, 2020-10-15)
- Design PR: [ZcashFoundation/zebra#0000](https://github.com/ZcashFoundation/zebra/pull/0000)
- Zebra Issue: [ZcashFoundation/zebra#0000](https://github.com/ZcashFoundation/zebra/issues/0000)

# Summary
[summary]: #summary

Handling blockchain scanning in subtasks via a zebra-client component in zebrad, and
a wallet component in zebra-cli to query that component for wallet balances and to construct
transactions and send them out via zebrad.

# Motivation
[motivation]: #motivation

We want to send money via Zebra efficiently and securely.

# Definitions
[definitions]: #definitions


# Guide-level explanation
[guide-level-explanation]: #guide-level-explanation

<!-- Explain the proposal as if it was already included in the project and you were teaching it to another Zebra programmer. That generally means: -->

<!-- - Introducing new named concepts. -->
<!-- - Explaining the feature largely in terms of examples. -->
<!-- - Explaining how Zebra programmers should *think* about the feature, and how it should impact the way they use Zebra. It should explain the impact as concretely as possible. -->
<!-- - If applicable, provide sample error messages, deprecation warnings, migration guidance, or test strategies. -->
<!-- - If applicable, describe the differences between teaching this to existing Zebra programmers and new Zebra programmers. -->

<!-- For implementation-oriented RFCs (e.g. for Zebra internals), this section should focus on how Zebra contributors should think about the change, and give examples of its concrete impact. For policy RFCs, this section should provide an example-driven introduction to the policy, and explain its impact in concrete terms. -->

zebra-client
- client component
- (does: key access, blockchain scanning)
- (has a whole separate sled DB for ’users’s txs, keys, etc)
- (keeps track of where it’s scanned to)
- (can be executed in its own tokio subtask to isolate from timing)
- (has access to notifs from zebra-state about new blocks etc)
- (lives as a subtask of zebrad)
- zebra-cli talks to this subcomponent which is in a running zebrad
- (the scanning executes independently (but in the same process) of the normal node operation)
- (zebra-client, in side zebrad, runs in its own separate task, in case it crashes, it’s not noticeable)
zebra-cli
- (can use servo/bincode + servo/ipc-channel to communicate with zebrad)
- (separate processes)

# Reference-level explanation
[reference-level-explanation]: #reference-level-explanation

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

Supporting a wallet assumes risk.

# Rationale and alternatives
[rationale-and-alternatives]: #rationale-and-alternatives

<!-- - What makes this design a good design? -->
<!-- - Is this design a good basis for later designs or implementations? -->
<!-- - What other designs have been considered and what is the rationale for not choosing them? -->

> What is the impact of not doing this?
We can't send money with zebra alone.

# Prior art
[prior-art]: #prior-art

<!-- Discuss prior art, both the good and the bad, in relation to this proposal. -->
<!-- A few examples of what this can include are: -->

<!-- - For community proposals: Is this done by some other community and what were their experiences with it? -->
<!-- - For other teams: What lessons can we learn from what other communities have done here? -->
<!-- - Papers: Are there any published papers or great posts that discuss this? If you have some relevant papers to refer to, this can serve as a more detailed theoretical background. -->

<!-- This section is intended to encourage you as an author to think about the lessons from other projects, to provide readers of your RFC with a fuller picture. -->
<!-- If there is no prior art, that is fine - your ideas are interesting to us whether they are brand new or if they are an adaptation from other projects. -->

<!-- Note that while precedent set by other projects is some motivation, it does not on its own motivate an RFC. -->
<!-- Please also take into consideration that Zebra sometimes intentionally diverges from common Zcash features and designs -->.

# Unresolved questions
[unresolved-questions]: #unresolved-questions

<!-- - What parts of the design do you expect to resolve through the RFC process before this gets merged? -->
<!-- - What parts of the design do you expect to resolve through the implementation of this feature before stabilization? -->
<!-- - What related issues do you consider out of scope for this RFC that could be addressed in the future independently of the solution that comes out of this RFC? -->

# Future possibilities
[future-possibilities]: #future-possibilities

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

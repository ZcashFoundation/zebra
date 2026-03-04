- Feature Name: basic_integration_testing
- Start Date: 2020-09-04
- Design PR: [ZcashFoundation/zebra#0000](https://github.com/ZcashFoundation/zebra/pull/0000)
- Zebra Issue: [ZcashFoundation/zebra#1004](https://github.com/ZcashFoundation/zebra/issues/1004)

# Summary

[summary]: #summary

Zebra's basic integration tests make sure that it works when deployed on mainnet and testnet.

Basic acceptance tests:

- genesis tests for basic network integration
- restart with existing state
- quick tests for a few thousand blocks
  - can be unstable on the current testnet
- `checkpoint_sync = true`
  - not yet implemented

On `main` branch merge:

- slow tests past the canopy mandatory checkpoint, for full block verification
- slow tests to the tip of the chain

For an up-to-date list, see:

- <https://github.com/ZcashFoundation/zebra/blob/main/zebrad/tests/acceptance.rs>
- <https://github.com/ZcashFoundation/zebra/tree/main/.github/workflows>

Design strategies:

- we write Rust-based acceptance tests, not shell scripts
- use #[ignored] to disable long-running integration tests by default
- use a feature to disable integration tests that need pre-cached state

Logging plans:

- limit bundled logs to the first N lines, and the last N lines ?
- write logs to a file ?
- work out how to avoid filling up the child stdout and stderr pipes
- keep warning and error logs
- only keep initial and final info/debug/trace logs

# Motivation

[motivation]: #motivation

To develop and deploy Zebra, we need to make sure that its sync, verify, and state functionality works.

These tests help us be confident that PRs and main branch merges don't break Zebra's basic functionality.

# Definitions

[definitions]: #definitions

<!-- Lay out explicit definitions of any terms that are newly introduced or which cause confusion during the RFC design process. -->

# Guide-level explanation

[guide-level-explanation]: #guide-level-explanation

<!-- Explain the proposal as if it was already included in the project and you were teaching it to another Zebra programmer. That generally means:

- Introducing new named concepts.
- Explaining the feature largely in terms of examples.
- Explaining how Zebra programmers should *think* about the feature, and how it should impact the way they use Zebra. It should explain the impact as concretely as possible.
- If applicable, provide sample error messages, deprecation warnings, migration guidance, or test strategies.
- If applicable, describe the differences between teaching this to existing Zebra programmers and new Zebra programmers.

For implementation-oriented RFCs (e.g. for Zebra internals), this section should focus on how Zebra contributors should think about the change, and give examples of its concrete impact. For policy RFCs, this section should provide an example-driven introduction to the policy, and explain its impact in concrete terms. -->

# Reference-level explanation

[reference-level-explanation]: #reference-level-explanation

<!-- This is the technical portion of the RFC. Explain the design in sufficient detail that:

- Its interaction with other features is clear.
- It is reasonably clear how the feature would be implemented, tested, monitored, and maintained.
- Corner cases are dissected by example.

The section should return to the examples given in the previous section, and explain more fully how the detailed proposal makes those examples work. -->

# Drawbacks

[drawbacks]: #drawbacks

<!-- Why should we *not* do this? -->

# Rationale and alternatives

[rationale-and-alternatives]: #rationale-and-alternatives

<!--
- What makes this design a good design?
- Is this design a good basis for later designs or implementations?
- What other designs have been considered and what is the rationale for not choosing them?
- What is the impact of not doing this?
-->

# Prior art

[prior-art]: #prior-art

<!--
Discuss prior art, both the good and the bad, in relation to this proposal.
A few examples of what this can include are:

- For community proposals: Is this done by some other community and what were their experiences with it?
- For other teams: What lessons can we learn from what other communities have done here?
- Papers: Are there any published papers or great posts that discuss this? If you have some relevant papers to refer to, this can serve as a more detailed theoretical background.

This section is intended to encourage you as an author to think about the lessons from other projects, to provide readers of your RFC with a fuller picture.
If there is no prior art, that is fine - your ideas are interesting to us whether they are brand new or if they are an adaptation from other projects.

Note that while precedent set by other projects is some motivation, it does not on its own motivate an RFC.
Please also take into consideration that Zebra sometimes intentionally diverges from common Zcash features and designs.
-->

# Unresolved questions

[unresolved-questions]: #unresolved-questions

<!--
- concern over the full test using a timeout as a failure criteria introducing spurious failures

- What parts of the design do you expect to resolve through the RFC process before this gets merged?
- What parts of the design do you expect to resolve through the implementation of this feature before stabilization?
- What related issues do you consider out of scope for this RFC that could be addressed in the future independently of the solution that comes out of this RFC?
-->

# Future possibilities

[future-possibilities]: #future-possibilities

<!--
Think about what the natural extension and evolution of your proposal would
be and how it would affect Zebra and Zcash as a whole. Try to use this
section as a tool to more fully consider all possible
interactions with the project and cryptocurrency ecosystem in your proposal.
Also consider how this all fits into the roadmap for the project
and of the relevant sub-team.

This is also a good place to "dump ideas", if they are out of scope for the
RFC you are writing but otherwise related.

If you have tried and cannot think of any future possibilities,
you may simply state that you cannot think of anything.

Note that having something written down in the future-possibilities section
is not a reason to accept the current or a future RFC; such notes should be
in the section on motivation or rationale in this or subsequent RFCs.
The section merely provides additional information.
-->

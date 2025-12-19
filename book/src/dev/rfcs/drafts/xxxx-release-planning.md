- Feature Name: `release_planning`
- Start Date: 2020-09-14
- Design PR: [ZcashFoundation/zebra#1063](https://github.com/ZcashFoundation/zebra/pull/1063)
- Zebra Issue: [ZcashFoundation/zebra#1963](https://github.com/ZcashFoundation/zebra/issues/1963)

# Draft

Note: This is a draft Zebra RFC. See
[ZcashFoundation/zebra#1963](https://github.com/ZcashFoundation/zebra/issues/1963)
for more details.

# Summary

[summary]: #summary

Release and distribution plans for Zebra.

# Motivation

[motivation]: #motivation

We need to plan our release and distribution processes for Zebra. Since these
processes determine how users get Zebra and place constraints on how we do
Zebra development, it's important to think through the implications of our
process choices.

# Guide-level explanation

[guide-level-explanation]: #guide-level-explanation

Zebra is developed _library-first_, as a collection of independently useful
Rust crates composed together to create `zebrad`, the full node implementation.
This means that our release and distribution processes need to handle
distribution of the `zebra-*` libraries, as well as of `zebrad`.

The official distribution channels are as follows:

- `zebra-*` libraries are distributed via Cargo, using
  [crates.io](https://crates.io);
- `zebrad` is distributed in binary form via Docker images generated in CI
  (binaries) or in source form via `cargo install`.

The release process is controlled by pushing an appropriate tag to the
`ZcashFoundation/zebra` git repository.

# Reference-level explanation

[reference-level-explanation]: #reference-level-explanation

(This section should describe the mechanics of the release process in more
detail, once we have agreement on distribution channels. Currently, one
suggestion is described and motivated below).

# Rationale and alternatives

[rationale-and-alternatives]: #rationale-and-alternatives

## Versioning

We previously agreed on a tentative versioning policy for `zebrad` and the
component `zebra-` libraries. In both cases, we follow semver rules. For
`zebrad`, we plan to align the major version number with the NU number, so that
mainnet NU3 corresponds to `3.x`, mainnet NU4 to `4.x`, etc. For the `zebra-` libraries, we
commit to following semver rules, but plan to increment the major versions as
fast as we need to implement the features we want.

## Distribution Channels

To handle releases of the component `zebra-` libraries, there's a clear best
answer: publish the libraries to crates.io, so that they can be used by other
Rust projects by adding an appropriate line to the `Cargo.toml`.

For `zebrad` the situation is somewhat different, because it is an application,
not a library. Because Zcash is a living protocol and `zebrad` is living
software, whatever process we choose must consider how it handles updates.
Broadly speaking, possible processes can be divided into three categories:

1. Do not expect our users to update their software and do not provide them a
    means to do so;

2. Use an existing update / deployment mechanism to distribute our software;

3. Write our own update / deployment mechanism to distribute our software.

The first category is mentioned for completeness, but we need to provide users
with a way to update their software. Unfortunately, this means that standalone
binaries without an update mechanism are not a workable option for us. The
third category is also unfavorable, because it creates a large amount of work
for a task that is not really the focus of our product. This suggests that we
focus on solutions in the second category.

One solution in the second category is to publish Docker images. This has a
number of attractive features. First, we already produce Docker images for our
own cloud deployments, so there is little-to-no marginal effort required to
produce these for others as a distribution mechanism. Second, providing Docker
images will make it easier for us to provide a collection of related software
in the future (e.g., providing an easy-to-deploy Prometheus / Grafana instance,
or a sidecar Tor instance). Third, Docker has a solid upgrade story, and we
can instruct users to use the `:latest` version of the Docker image or steer
them to auto-update mechanisms like Watchtower.

While this solution works well for cloud deployments, Docker is not suitable
everywhere. What should we do outside of Docker? One solution would be to try
to create packages for each platform-specific package manager (Homebrew,
something for Windows, various different flavors of Linux distribution), but
this creates a large amount of additional work requiring platform-specific
knowledge. Worse, this work cannot be outsourced to others without giving up
control over our software distribution -- if, for instance, a third party
creates a Homebrew package, and we recommend people install Zebra using that
package, we're reliant on that third party to continue packaging our software
forever, or leave our users stranded.

Instead, we can publish `zebrad` as a Rust crate and recommend `cargo install`.
This approach has two major downsides. First, installation takes longer,
because Zebra is compiled locally. Second, as long as we have a dependency on
`zcashconsensus`, we'll have to instruct users to install some
platform-specific equivalent of a `build-essential` package. And as long as
we depend on `zcash_script`, we'll have to instruct users to install `libclang`.
However, even for
crates such as `zcashconsensus` that build native code, the `cargo`-managed
build process is far, far more reliable than build processes for C or C++
projects. We would not be asking users to run autotools or `./configure`, just
a one-step `cargo install`. We also know that it's possible to reliably build
Zebra on each platform with minimal additional steps, because we do so in our
CI.

In contrast to these downsides, distributing `zebra` through Cargo has a number
of upsides. First, because we distribute our libraries using crates.io, we
already have to manage tooling for publishing to crates.io, so there's no
additional work required to publish `zebrad` this way. Second, we get a
cross-platform update mechanism with no additional work, since `cargo install`
will upgrade to the latest published version. Third, we don't rely on any
third parties to mediate the relationship between us and our users, so users
can get updates as soon as we publish them. Fourth, unlike a system package
manager, we can pin exact hashes of every transitive dependency (via the
`Cargo.lock`, which `cargo install` can be configured to respect). Fifth,
we're positioned to pick up (or contribute to) ecosystem-wide integrity
improvements like a transparency log for `crates.io` or work on reproducible
builds for Rust.

This proposal is summarized above in the [guide-level
explanation](#guide-level-explanation).

## Release Processes

The next question is what kind of release processes and automation we should
use. Here are two important priorities for these processes:

1. Reducing the friction of doing any individual release, allowing us to move
    closer to a continuous deployment model;
2. Reducing the risk of error in the release process.

These are roughly in order of priority but they're clearly related, since the
more friction we have in the release process, the greater the risk of error,
and the greater the risk of error, the more friction we require to prevent it.

Automation helps to reduce friction and to reduce the risk of error, but
although it helps enormously, it has declining returns after some point (in the
sense that automating the final 5% of the work is significantly more complex
and error-prone than automating the first 5% of the work). So the challenge is
to find the right "entry point" for the automated part of the system that
strikes the right balance.

One possibility is the following. CD automation is triggered by pushing a new
tag to the git repository. Tags are specified as `crate-semver`, e.g.
`zebra-network-3.2.8`, `zebrad-3.9.1`, etc. When a new tag is pushed, the CD
automation parses the tag to determine the crate name. If it is `zebrad`, it
builds a new Docker image and publishes the image and the crate. Otherwise, it
just publishes the crate.

To publish a new version of any component crate, the process is:

1. Edit the `Cargo.toml` to increment the version number;
2. Update `crate/CHANGELOG.md` with a few human-readable sentences describing
    changes since the last release (examples: [cl1], [cl2], [cl3]);
3. Submit a PR with these changes;
4. Tag the merge commit and push the tag to the git repo.

[cl1]: https://github.com/ZcashFoundation/ed25519-zebra/blob/main/CHANGELOG.md
[cl2]: https://github.com/dalek-cryptography/x25519-dalek/blob/master/CHANGELOG.md
[cl3]: https://github.com/dalek-cryptography/curve25519-dalek/blob/main/curve25519-dalek/CHANGELOG.md

All subsequent steps (publishing to crates.io, building docker images, etc) are
fully automated.

Why make these choices?

- Triggering on a tag, rather than some other trigger, ensures that each
  deployment corresponds to a particular, known, state of the source
  repository.

- Editing the version number in the `Cargo.toml` should be done manually,
  because it is the source of truth for the crate version.

- Changelog entries should be written by hand, rather than auto-generated from
  `git log`, because the information useful as part of a changelog is generally
  not the same as the information useful as part of commit messages. (If this
  were not the case, changelogs would not be useful, because `git log` already
  exists). Writing the changelog entries by hand would be a burden if we
  queued a massive set of changes between releases, but because releases are
  low-friction and we control the distribution channel, we can avoid this
  problem by releasing frequently, on a weekly or daily basis.

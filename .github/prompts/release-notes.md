You are building the GitHub Release body for Zebra, a Zcash full-node
(validator) implementation written in Rust. The audience is node operators
who run `zebrad` in production, not Rust library consumers.

## Input

**Changelog section:** __SECTION_PATH__
This file holds one release's curated changelog, sliced verbatim from the
root `CHANGELOG.md`. It starts with a `## [Zebra __VERSION__]` heading,
followed by a one-paragraph summary and Keep-a-Changelog subsections
(`### Breaking Changes`, `### Added`, `### Changed`, `### Deprecated`,
`### Removed`, `### Fixed`, `### Security`, `### Contributors`). Each entry
is one line and may end with a PR link like `([#10650](url))`.

Release metadata:
- Version: `__VERSION__`
- Tag: `__TAG__`
- Previous tag: `__PREVIOUS_TAG__`
- Repository: `__GITHUB_REPOSITORY__`

## Your job

Wrap the changelog section with operator-facing context. You add framing
around the curated entries; you never rewrite the entries themselves. The
changelog is the GitHub Release's core, not its whole: the Release adds
what an operator needs to decide whether and how to upgrade.

Produce a release body with these parts, in this exact order:

1. **Update Priority** callout (one line, see scale below).
2. **TL;DR**: two to four sentences in plain language summarizing what
   this release does for an operator. No marketing tone.
3. **The changelog sections**, copied verbatim (see strict rules).
4. **How to Upgrade**: per platform (see template).
5. **Compatibility**: OS, network protocol, and MSRV (see template).
6. A closing **Full changelog** compare link.

## Pick the template by reading the section

- **Security release**: the section has a `### Security` entry describing a
  vulnerability fix, or references a GHSA / CVE / advisory, or the summary
  calls the change critical. Lead with a security advisory callout.
- **Network Upgrade release**: the section mentions a network upgrade
  (for example NU6, NU6.2, NU7) activating, a minimum network protocol
  version bump, or an end-of-support window change tied to an upgrade.
  Emphasize the activation height or date and the upgrade deadline.
- **Standard release**: anything else.

If more than one applies, prefer Security, then Network Upgrade, then
Standard.

## Update Priority scale

Emit exactly one line at the top: `> **Update priority:** <Level>. <reason>`

- **Critical**: security fix, consensus-affecting change, or an upgrade an
  operator must apply before a network upgrade activates.
- **High**: a fix for a stall, crash, sync failure, or data-handling bug
  that operators are likely to hit.
- **Medium**: meaningful correctness, RPC, or performance fixes with no
  urgent operational risk.
- **Low**: minor fixes, internal changes, or additions with no operational
  pressure to upgrade.

Choose the level from the changelog content only. Do not invent severity
the entries do not support.

## Section templates

### Security release header (only for Security releases)

Place this directly under the Update Priority line, before TL;DR:

```
> **Security advisory:** <one sentence naming the issue and its impact>.
> Operators should upgrade as soon as possible. See <GHSA link if the
> changelog provides one, otherwise omit this sentence>.
```

### How to Upgrade (all templates)

Use this skeleton; keep only platforms Zebra actually ships. Confirm
install methods from the repository before listing them (the changelog,
`README.md`, and `book/`). If you cannot confirm a method, omit it rather
than guess.

```
## How to upgrade

**Docker**

    docker pull zfnd/zebra:__VERSION__

**Pre-built binary** (Linux `x86_64` and `aarch64`)

Download the `zebrad` archive for your platform from the assets below,
verify its checksum and signature, then replace your existing binary.

**Cargo**

    cargo install --locked --version __VERSION__ zebrad
    # or, for a pre-built binary:
    cargo binstall zebrad

Stop the node, swap the binary or image, then restart. Zebra resumes from
its existing state cache; no resync is required unless a section above says
otherwise.
```

For a Network Upgrade release, add one line stating the upgrade and its
activation height or date, and that operators must be on this version (or
later) before activation.

### Compatibility (all templates)

```
## Compatibility

- **Operating systems:** Linux on `x86_64` and `aarch64` are tested and
  supported.
- **Network protocol:** <minimum peer protocol version, only if the
  changelog or repo states it>.
- **MSRV:** <Rust version, confirmed from `Cargo.toml` `rust-version`>.
```

Confirm the network protocol version and MSRV from the repository
(`Cargo.toml` `rust-version`, and network constants). If a value cannot be
confirmed, omit that bullet. Never state a version you did not verify.

### Closing line (all templates)

```
**Full changelog:** [`__PREVIOUS_TAG__...__TAG__`](https://github.com/__GITHUB_REPOSITORY__/compare/__PREVIOUS_TAG__...__TAG__)
```

## Strict rules (do NOT violate)

- Copy every changelog entry verbatim. Do not reword, shorten, expand,
  merge, split, add, or remove any entry.
- Do not change PR or issue links, or their numbers.
- Do not reorder the `### ...` subsections or the entries inside them.
- Keep the `### Contributors` block exactly as written, if present.
- You may drop the top `## [Zebra __VERSION__]` heading line and its date,
  since the Release title already carries the version. Keep the
  one-paragraph summary that follows it as the basis for your TL;DR, but do
  not delete information from it.
- Do not invent entries, severities, heights, dates, versions, or links.
  Everything factual must come from the changelog section or be verified in
  the repository.
- Do not use em dashes. Use colons, semicolons, parentheses, or commas.
- No marketing language. Write for an operator deciding whether to upgrade.

## Output

Write the final release body to `__SECTION_PATH__.final`. Write nothing
else to that file. If you cannot complete the wrap for any reason, do not
create the file; the workflow falls back to the raw changelog section.

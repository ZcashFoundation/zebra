# Changelog Guidelines for Zebra

Zebra has two types of changelogs with different audiences:

| Changelog | Audience | Location |
| --- | --- | --- |
| `zebrad` (binary) | Node operators | `CHANGELOG.md` (root) |
| Crates (libraries) | Rust developers | `<crate>/CHANGELOG.md` |

This document covers guidelines common to both, then specific guidance for each.

## CHANGELOG.md vs GitHub Release

These serve different purposes:

| | `CHANGELOG.md` | GitHub Release |
| --- | --- | --- |
| Purpose | Historical record of changes | Release announcement |
| Question answered | "What changed?" | "Should I upgrade? How?" |
| Content | Change entries with PR links | Changelog plus operational context |
| Audience focus | What is different | What to do about it |

The GitHub release wraps `CHANGELOG.md` content with operational context:

```text
GitHub Release
  - Update Priority (who should upgrade)
  - Highlights (TL;DR)
  - CHANGELOG.md content (Breaking Changes, Added, etc.)
  - How to Upgrade (per-platform steps)
  - Compatibility (OS, protocol, MSRV)
```

See [Part 4: GitHub Release Format](#part-4-github-release-format) for templates.

## Part 1: Common Guidelines

These apply to all Zebra changelogs regardless of audience.

### One entry per distinct change

Each distinct user-visible change gets **one entry on one line**. A PR that makes several independent changes gets one entry for each. Do not split a single change across multiple entries, and do not group multiple PRs on one line.

Good:

```text
- New `Bounded` vec type for compile-time size constraints ([#10056](link))
- Prometheus metrics for RocksDB I/O latency and compaction ([#10181](link))
```

Bad, one change split across entries:

```text
- Added mempool standardness checks ([#10224](link))
- Added `max_datacarrier_bytes` config option ([#10224](link))
```

Bad, multiple PRs on one line:

```text
- Prometheus metrics for monitoring ([#10175](link), [#10179](link), [#10181](link))
```

### Section order

Use [Keep a Changelog](https://keepachangelog.com/en/1.0.0/) section order:

```text
## [Version X.Y.Z](link) - YYYY-MM-DD

[2-4 sentence summary]

### Breaking Changes
### Added
### Changed
### Deprecated
### Removed
### Fixed
### Security
```

Only include sections that have entries.

### Section priority

When a single change touches multiple categories, place it in **one section only** based on the most impactful aspect. Priority order:

1. Breaking Changes
2. Security
3. Removed
4. Changed
5. Deprecated
6. Added
7. Fixed

Exception for crate changelogs: a change that is both breaking and additive (for example, adding a variant to a public enum that is not `#[non_exhaustive]`) is listed under Breaking Changes for its impact and under Added for the new capability, so consumers see both signals.

Fixed vs Changed: use Fixed only when the fix is invisible, meaning the bug is gone but output looks the same. If users see anything different (error messages, logs, behavior), use Changed.

### Tone and language

Write plainly and factually. Avoid:

- Hyperbolic adjectives: "comprehensive", "significant", "major", "awesome", "powerful"
- Vague intensifiers: "greatly improved", "much better", "highly optimized"
- Marketing language: "exciting new feature", "game-changing", "cutting-edge"
- Unnecessary hedging: "helps to", "aims to", "tries to"

### Frame by experience, not code

Describe what users experience, not what the code does.

| Code-focused (bad) | Experience-focused (good) |
| --- | --- |
| "Added mempool standardness checks" | "Mempool now rejects non-standard transactions" |
| "Implemented new error type" | "Error messages now show specific failure reason" |
| "Refactored config parsing" | "Config files now support X format" |
| "Changed function signature" | "`parse_block()` now returns `Result<Block, ParseError>`" |

## Part 2: zebrad Changelog (Node Operators)

File: `CHANGELOG.md` (repository root)

Audience: people who run `zebrad`: deploy nodes, write config files, use RPC endpoints, monitor via metrics, run Docker containers. May not be Rust developers.

### What is breaking for node operators?

A change is breaking if upgrading `zebrad` could cause existing setups to fail:

| Category | Example | Why it breaks |
| --- | --- | --- |
| Config field removed | Removing `[rpc].some_field` | Existing config files fail to parse |
| Config field renamed | `cache_dir` to `state_dir` | Existing config files fail to parse |
| Config field type changed | `port: "8233"` to `port: 8233` | Existing config files may fail |
| Environment variable renamed | `ZEBRA_RPC_PORT` to `ZEBRA_RPC__LISTEN_PORT` | Scripts and systemd units stop working |
| CLI argument removed/renamed | `--config` to `--config-file` | Scripts fail |
| RPC response format changed | `timestamp: "2025-01-01"` to `timestamp: 1735689600` | RPC consumers break |
| RPC endpoint removed | Removing `getinfo` | RPC consumers break |
| Database format (no downgrade) | New state version | Cannot revert to previous version |
| Default behavior change | Mempool rejects previously accepted tx | Mining/wallet software may break |

### What is NOT breaking for node operators

| Category | Example | Why it is safe |
| --- | --- | --- |
| New optional config field | Adding `max_datacarrier_bytes` with default | Existing configs work |
| New RPC endpoint | Adding `getmempoolinfo` | Existing consumers unaffected |
| New RPC response field | Adding `pingtime` to `getpeerinfo` | Consumers ignore new fields |
| Library API changes | `pub const` to `pub(crate) const` | Operators do not use Rust APIs |
| Internal refactors | Changing error types internally | No external interface change |
| Performance improvements | Avoiding clones, faster verification | Same behavior, just faster |
| New metrics | Adding Prometheus metrics | Opt-in observability |

### What NOT to include in the zebrad changelog

| Type | Why exclude |
| --- | --- |
| Library API changes | Put in crate CHANGELOG |
| Internal refactors | No operator-visible effect |
| CI/workflow changes | No runtime impact |
| Test changes | No runtime impact |
| Dependency bumps | Unless security fixes |

Red flags an entry does not belong:

- "No changes required to existing deployments"
- Describing implementation without operator impact
- Entry only makes sense to someone reading code

### Release summary (zebrad)

- 2-4 sentences maximum
- Lead with the most impactful change
- Name config options, RPC methods, environment variables
- Focus on what operators need to know or do

Good:

```text
Mempool now enforces zcashd-compatible standardness checks; transactions with
non-standard scripts are rejected. Configure OP_RETURN limits with
`max_datacarrier_bytes` in `[mempool]`. This release also adds Prometheus
metrics for value pools, sync progress, and RPC latencies.
```

Bad:

```text
This release refactors internal error handling, updates dependencies, and
improves code organization across multiple crates.
```

### Entry examples (zebrad)

Good:

```text
- Mempool now rejects transactions with non-standard transparent scripts.
  Configure OP_RETURN limits with `max_datacarrier_bytes` in `[mempool]`
  (default: 83 bytes) ([#10224](link))
```

Bad:

```text
- Added mempool standardness checks (#10224)
```

Breaking change format:

```text
### Breaking Changes

- Environment variables now use `ZEBRA_SECTION__KEY` format (double underscore).
  Update scripts: `ZEBRA_RPC_PORT` to `ZEBRA_RPC__LISTEN_ADDR` ([#9768](link))
```

### Network upgrade releases

For releases activating a network upgrade, use a dedicated section:

```text
## [Zebra X.Y.Z](link) - YYYY-MM-DD

**This release activates [NU Name] on Zcash mainnet.**

| | |
|-|-|
| Activation Height | X,XXX,XXX |
| Expected Date | ~YYYY-MM-DD |
| Specifications | [ZIP 2XX](https://zips.z.cash/zip-02xx), [ZIP 3XX](https://zips.z.cash/zip-03xx) |

Upgrade before block X,XXX,XXX to remain on the Zcash network.

### Network Upgrade

- Implements [ZIP 2XX: Title](https://zips.z.cash/zip-02xx) ([#XXXX](link))
- Implements [ZIP 3XX: Title](https://zips.z.cash/zip-03xx) ([#YYYY](link))

### Added
...
```

Always include:

- Activation height
- Approximate activation date
- Links to all ZIPs being deployed
- Clear upgrade deadline

### ZIP references

Always link to ZIPs when implementing consensus or protocol changes:

```text
- Implements [ZIP 317: Proportional Transfer Fee Mechanism](https://zips.z.cash/zip-0317) ([#9876](link))
```

Use the canonical URL format: `https://zips.z.cash/zip-XXXX`

### Deprecations

For features being phased out, document the timeline:

```text
### Deprecated

- `getinfo` RPC is deprecated and will be removed in v5.0.0. Use `getblockchaininfo`,
  `getnetworkinfo`, and `getwalletinfo` instead ([#10XXX](link))
```

If following a staged deprecation (like zcashd):

1. Stage 1: enabled by default, can disable with flag
2. Stage 2: disabled by default, can enable with flag
3. Stage 3: removed entirely

### Security releases

For releases addressing security issues:

```text
## [Zebra X.Y.Z](link) - YYYY-MM-DD - Security Release

**This is a security release. Immediate upgrade is recommended.**

This release addresses security issues reported through our disclosure process.
Details will be published after sufficient upgrade adoption.

| Severity | Affected Versions |
|----------|-------------------|
| High | < X.Y.Z |

### Security

- Fixed denial-of-service vulnerability in peer message handling ([#XXXX](link))
```

Minimize technical details in the public changelog. Link to the security advisory after the disclosure period.

## Part 3: Crate Changelogs (Library Consumers)

Files: `zebra-chain/CHANGELOG.md`, `zebra-consensus/CHANGELOG.md`, and so on.

Audience: Rust developers building on Zebra crates (Zaino, Zallet, other wallets and tools). These are technical users who read Rust code.

### What is breaking for library consumers?

A change is breaking if code using the crate will fail to compile or behave differently:

| Category | Example | Migration required |
| --- | --- | --- |
| Public type removed | Removing `pub struct BlockHash` | Find replacement or copy type |
| Public type renamed | `BlockHash` to `BlockHeaderHash` | Update all usages |
| Function signature changed | `fn parse(data: &[u8])` to `fn parse(data: Vec<u8>)` | Update call sites |
| Return type changed | `fn get() -> Hash` to `fn get() -> Option<Hash>` | Handle new return type |
| Trait bounds added | `fn process<T>()` to `fn process<T: Clone>()` | Ensure types implement trait |
| Public field removed | Removing `block.header` | Use accessor method |
| Visibility reduced | `pub const X` to `pub(crate) const X` | Copy constant or request re-export |
| Error type changed | `ParseError` to `BlockParseError` | Update error handling |
| Feature flag required | Function now behind `#[cfg(feature = "x")]` | Enable feature in Cargo.toml |
| MSRV increased | 1.70 to 1.75 | Update toolchain |

Adding a variant to a public enum that is not marked `#[non_exhaustive]` is also breaking: downstream `match` expressions that were exhaustive stop compiling.

### What is NOT breaking for library consumers

| Category | Example | Why it is safe |
| --- | --- | --- |
| New public type | Adding `pub struct NewType` | Existing code unaffected |
| New public function | Adding `pub fn new_helper()` | Existing code unaffected |
| New trait impl | `impl Display for Block` | Existing code unaffected |
| New optional parameter | Adding `Option<Config>` with default | Existing calls work |
| Performance improvements | Internal optimization | Same API, faster |
| Documentation changes | Better rustdoc | No code changes needed |

### Release summary (crates)

- 2-4 sentences maximum
- Lead with breaking changes if any
- Name specific types, traits, functions affected
- Focus on what code changes are needed

Good:

```text
`FundingStreamReceiver` variants renamed for clarity: `Ecc` to `Bootstrap`,
`ZcashFoundation` to `Foundation`. The `subsidy` module constants are now
`pub(crate)`; use the provided accessor functions instead.
```

Bad:

```text
This release improves code organization and internal structure.
```

### Entry examples (crates)

Good, breaking change:

```text
### Breaking Changes

- `subsidy::MAX_BLOCK_SUBSIDY` is now `pub(crate)`. Use `Block::max_subsidy()`
  instead ([#10185](link))
```

Good, API addition:

```text
### Added

- `impl From<BlockHeader> for BlockHeaderHash` for convenient conversion ([#10056](link))
```

Good, type change:

```text
### Changed

- `AdjustedDifficulty::new()` now takes `Bounded<CompactDifficulty, 128>` instead
  of `Vec<CompactDifficulty>` for compile-time size validation ([#10056](link))
```

### Crate releases vs zebrad releases

Crate releases and zebrad releases are **independent**. A single PR may appear in both changelogs, documented differently for each audience.

Example: PR #10224 adds mempool standardness checks.

In `zebra-chain/CHANGELOG.md` (when zebra-chain 1.x.0 releases):

```text
### Added
- `StandardScript` type for validating transparent script standardness ([#10224](link))
```

In `CHANGELOG.md` (when zebrad 4.1.0 releases):

```text
### Changed
- Mempool now rejects transactions with non-standard transparent scripts.
  Configure OP_RETURN limits with `max_datacarrier_bytes` in `[mempool]` ([#10224](link))
```

Same PR, different perspectives:

- Crate changelog: what API changed (for developers updating their code)
- zebrad changelog: what behavior changed (for operators updating their setup)

### Deciding where to document

| Change type | Crate CHANGELOG | zebrad CHANGELOG |
| --- | --- | --- |
| Config option added/changed/removed | No | Yes |
| RPC endpoint added/changed/removed | No | Yes |
| CLI argument changed | No | Yes |
| Public Rust API changed | Yes | Only if operator impact |
| Public type/trait changed | Yes | Only if operator impact |
| Internal refactor | No | No |
| Performance improvement | If API-relevant | If user-noticeable |
| Bug fix | If API-relevant | If operator-noticeable |

A change can appear in both if it affects both audiences. Document it appropriately for each:

- Crate: focus on types, functions, migration code
- zebrad: focus on config, behavior, what operators see

If unsure, ask "Who needs to take action?":

- Developers changing Rust code: crate CHANGELOG
- Operators changing config/scripts: zebrad CHANGELOG
- Both: both changelogs, written for each audience

## Part 4: GitHub Release Format

GitHub releases wrap `CHANGELOG.md` content with operational context. Do not just copy `CHANGELOG.md`: add the information operators need to decide whether and how to upgrade.

### Update priority

Add an Update Priority section for releases with varying urgency:

```text
## Update Priority

| User Type | Priority | Reason |
|-----------|----------|--------|
| Mining pools | **High** | Mempool policy changes affect block templates |
| Exchanges | Medium | New RPC fields available |
| General operators | Medium | Performance improvements |
```

Priority levels:

- Critical: security fix, upgrade immediately
- High: breaking changes or consensus updates
- Medium: new features or notable fixes
- Low: minor improvements, upgrade at convenience

### How to upgrade

Include platform-specific upgrade instructions:

````text
## How to Upgrade

### Binary/Package

1. Stop zebrad: `systemctl stop zebrad` or `Ctrl+C`
2. Wait for clean shutdown (check logs for "shutdown complete")
3. Replace binary or update via package manager
4. Start zebrad: `systemctl start zebrad`

### Docker

```bash
docker pull zfnd/zebra:X.Y.Z
# or update your compose file and:
docker compose pull && docker compose up -d
```
````

Building from source:

```bash
git fetch && git checkout vX.Y.Z
cargo build --release
```

### Compatibility

Document what this release works with:

```text
## Compatibility

| Component | Supported |
|-----------|-----------|
| Operating Systems | Linux (glibc 2.31+), macOS 13+ |
| Zcash Protocol | All upgrades through NU6.1 |
| zcashd RPC Compatibility | 5.x compatible |
| Minimum Rust (source builds) | 1.75+ |
```

### Release templates

Standard release:

```text
## Zebra X.Y.Z

[2-4 sentence summary from CHANGELOG]

## Update Priority

| User Type | Priority | Reason |
|-----------|----------|--------|
| ... | ... | ... |

---

[CHANGELOG.md content: Breaking Changes, Added, Changed, Fixed sections]

---

## How to Upgrade

[Platform-specific instructions]

## Compatibility

[Compatibility table]

## Contributors

[From CHANGELOG]
```

Network upgrade release:

```text
## Zebra X.Y.Z - [NU Name] Activation

**This release activates [NU Name] on Zcash mainnet at block X,XXX,XXX (~YYYY-MM-DD).**

⚠️ **All node operators must upgrade before the activation height.**

## Update Priority

| User Type | Priority |
|-----------|----------|
| All operators | **Critical** |

## Network Upgrade Details

| | |
|-|-|
| Activation Height | X,XXX,XXX |
| Expected Date | ~YYYY-MM-DD |
| Testnet Activation | Block Y,YYY,YYY (active since vA.B.C) |

### Implemented ZIPs

- [ZIP XXX: Title](https://zips.z.cash/zip-0xxx)
- [ZIP YYY: Title](https://zips.z.cash/zip-0yyy)

---

[CHANGELOG.md content]

---

## How to Upgrade

[Platform-specific instructions]

## Compatibility

[Compatibility table]
```

Security release:

```text
## Zebra X.Y.Z - Security Release

⚠️ **This is a security release. Immediate upgrade is recommended for all users.**

## Update Priority

| User Type | Priority |
|-----------|----------|
| All operators | **Critical** |

## Security

This release addresses [N] security issue(s) reported through our security
disclosure process.

| Issue | Severity | Affected Versions |
|-------|----------|-------------------|
| [Brief description] | High/Medium/Low | < X.Y.Z |

Full details will be published in a security advisory after sufficient upgrade
adoption.

---

[Other CHANGELOG.md content if any]

---

## How to Upgrade

[Platform-specific instructions, emphasize urgency]
```

### What NOT to include in a GitHub release

- Raw git log or commit lists
- Internal implementation details
- Entries that only matter to contributors
- Duplicate content (link to docs instead of repeating)

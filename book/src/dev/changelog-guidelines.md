# Changelog guidelines for Zebra

Document effects on operators and library consumers, not implementation details.

| Project | Audience | Generated file |
| --- | --- | --- |
| `zebrad` | Operators using configuration, CLI, RPC, metrics, or Docker | Root `CHANGELOG.md` |
| Library crate | Rust developers using its public API | `<crate>/CHANGELOG.md` |

## How changelogs are written

[Changie](https://changie.dev) generates every active changelog from `.changes/`.
Never hand-edit root or crate `CHANGELOG.md` files: the next `changie merge`
replaces them. The [configuration][changie-config] defines project keys,
output paths, and section order.

### Source pull requests

Use Changie v1.25.2, pinned by the release workflow:

```sh
changie new -j zebrad -k Changed
changie new -j zebra-chain -j zebra-state -k breaking
```

The commands prompt for a body; repeated `-j` flags reuse it for each project.
Use separate commands when wording differs by audience, or `-b 'entry text'`
for noninteractive input. Single shell quotes preserve Markdown backticks;
use the prompt for bodies containing apostrophes.

The body contains the entry text without a leading bullet or heading. Include a
link to the actual PR in the form `([#number](PR-URL))`; do not invent a PR number.
Commit the generated `.changes/unreleased/*.yaml` files with the change. Ordinary
source PRs do not batch releases or regenerate changelogs.

Use these configured project keys:

- `zebrad`
- `zebra-chain`, `zebra-consensus`, `zebra-network`, `zebra-node-services`
- `zebra-rpc`, `zebra-script`, `zebra-state`, `zebra-test`, `zebra-utils`
- `tower-batch-control`, `tower-fallback`

Use exact kind keys from the section-order table, not display headings.

The [PR gate][pr-gate] dry-runs every project's pending fragments on PRs, merge
groups, and `main`. Malformed fragments fail even when introduced by another PR.
Prefer `changie new` over handwritten YAML. In YAML single-quoted bodies, double
apostrophes; quote text containing colon-space or use a block scalar.

Maintainers can comment `/changelog` on a PR for a
[draft suggestion][changelog-command]. It requires the repository AI integration,
does not write to the branch, and reads at most 20,000 diff bytes. Review the full
PR for accuracy and coverage before accepting it.

### Release batching and corrections

The [release workflow][release-workflow] batches each project into the release
planner's exact version, then merges version files into generated changelogs.
Never substitute `changie batch auto` for the approved plan. For an approved
manual batch, set `PROJECT` to the project key and `VERSION` without a `v` prefix:

```sh
changie batch "v${VERSION}" --project "${PROJECT}"
changie merge
```

The result is `.changes/<project>/v<version>.md`. Correct an already-batched
entry, including a historical typo, in that version file and run `changie merge`.
Do not recreate consumed fragments as entries for the next release. Commit the
source and generated output together.

The [batching script][batch-script] restores `.changes/` from the Release PR's
base revision, overwriting branch-local curation. Land missing fragments on
`main` before refreshing; preserve and reapply version-file corrections after
the final refresh. Never rerun over unpreserved corrections.

The script adds a generic dependency entry only for released projects without
fragments. Audit API and dependency changes, including dependency-driven version
bumps, against each package's previous published release, not the last `zebrad`
release or the generated entries.

The command `changie latest -j zebra-state` prints a project-prefixed version,
such as `zebra-state-v13.0.0`. For `zebrad`, remove the `zebrad-` prefix to obtain
its Git tag; its tags have the form `v<version>`.

## CHANGELOG.md vs GitHub Release

A GitHub Release adds upgrade guidance to the changelog. The controller uses the
matching `zebrad` section of the generated root changelog as
[release notes][release-config]. Put summaries, upgrade instructions, and
compatibility information in `.changes/zebrad/v<version>.md`, then regenerate
before publication. Recovery can overwrite GitHub-only edits.

## Part 1: Common guidelines

### One entry per distinct change

Write one bullet per distinct change per affected project, keeping migration
instructions with it even across multiple lines. Never combine different PRs.

Describe the net change from the previous release. If an unreleased addition is
renamed, update its existing fragment. If it is removed before release, remove
the fragment rather than documenting an addition and removal nobody received.
Exclude defects introduced and fixed entirely between releases.

Documentation, verification-only changes, and internal refactors without a
consumer-visible effect do not need entries. An entry whose only point is that
existing deployments need no changes does not belong in the operator changelog.

### Section order and priority

The configured display order differs from the priority used to classify a change:

| Display order | CLI kind | Use for |
| --- | --- | --- |
| Breaking Changes | `breaking` | Incompatible interfaces or behavior, with migration guidance |
| Added | `Added` | New capabilities |
| Changed | `Changed` | Observable behavior, output, logs, or error changes |
| Deprecated | `Deprecated` | Features scheduled for removal, with replacement and timeline |
| Removed | `Removed` | Removed capabilities |
| Fixed | `Fixed` | Fixes invisible to users: the defect is gone but output is unchanged |
| Security | `Security` | Security fixes, coordinated with the disclosure process |

Choose the most impactful applicable category: breaking, Security, Removed,
Changed, Deprecated, Added, then Fixed. Do not repeat one change in several
sections. A breaking addition gets one `breaking` fragment that explains the
impact and names the new capability. Examples include a variant added to an
exhaustive public enum or a field added to a public struct constructed with
literals. Distinct changes in the same PR still need separate entries.

Only sections with entries are rendered. Do not add empty headings. Use the
version heading and date generated by Changie.

### Tone and language

Write factual, concise entries naming affected configuration keys, RPC methods,
types, or functions and what consumers observe or must change. Avoid
implementation narration, hyperbole, marketing, intensifiers, hedging, and em
dashes. Match the project's style.

Lead release summaries with the most consequential change, especially a break.
Name operational requirements for operators and API migration work for libraries.

### Gate coverage is not editorial policy

The source changelog gate checks PR types `feat`, `fix`, `perf`, `refactor`, and
`build`, plus titles declaring a break. It expects a fragment for every directly
changed publishable package, excluding documentation and verification files.
A root manifest change also triggers the `zebrad` requirement. A lockfile-only
change triggers it when the node's production dependency graph changes,
including supported non-default features and target-specific dependencies, but
excluding development dependencies and test-only features.

Do not invent operator-facing changes to satisfy this broader file-based check;
explain justified omissions to reviewers. Follow the
[breaking-change declaration rules](../CONTRIBUTING.md#breaking-changes).
A `breaking` fragment requires `!` in the PR title; a declared package break
requires a `breaking` fragment. Unlike advisory API and changelog coverage
checks, malformed-fragment batching failures block the aggregate.

## Part 2: zebrad changelog (node operators)

### What is breaking for node operators?

An operator-facing change is breaking when an existing deployment or client can
fail after upgrading. Explain the affected setup and the replacement.

| Interface | Breaking changes |
| --- | --- |
| Configuration | Removed, renamed, or retyped fields; incompatible defaults |
| Environment | Removed or renamed variables used by scripts or services |
| CLI | Removed or renamed arguments, incompatible parsing |
| RPC | Removed methods; incompatible parameters, responses, or semantics |
| State | A database-format change that prevents downgrade |
| Behavior | New defaults or policy that invalidate existing integrations |
| Build requirements | An increased minimum supported Rust version for source builds |

An optional configuration field with a compatible default, a new RPC method or
response field, new metrics, and a behavior-preserving optimization are not
inherently breaking. Describe actual compatibility constraints; do not assert
that every client tolerates an added response field.

Rust API changes belong in library changelogs unless they also affect operators.
Omit CI changes and ordinary dependency-version lists. Describe dependency
changes only for security fixes or concrete operational effects.

### Network upgrade releases and ZIP references

For an upgrade-support release, record:

- The upgrade name and network.
- Activation height and approximate activation date, clearly labeled as an
  estimate rather than a time-based activation condition.
- The deadline for upgrading and the consequence of missing it.
- Each implemented ZIP, linked using `https://zips.z.cash/zip-NNNN`.
- Testnet activation information and the first supporting release when relevant.

Link consensus or protocol changes to the corresponding ZIP or protocol
specification. Keep activation heights consistent with the release's network
parameters. Put operational context above the normal category sections; a
release-specific network-upgrade section is not a new Changie kind.

### Deprecations

Follow the [deprecation policy](release-process.md#deprecation). For staged
transitions, name the implemented stage: enabled by default with opt-out,
disabled by default with opt-in, or removed. Claim controls only when they exist.

### Security releases

State the affected versions, severity, upgrade urgency, and fixed version.
Keep public technical detail limited until coordinated disclosure permits it.
Link the advisory when published and credit reporters according to
[SECURITY.md][security]. An unpublished fix does not justify disclosing an
exploit in an ordinary PR fragment.

## Part 3: Crate changelogs (library consumers)

Describe affected public APIs and semantics, including fixes without signature
changes, and the migration for Rust callers.

### What is breaking for library consumers?

Apply [Cargo's SemVer guidance][cargo-semver]. Breaking changes include removed
or renamed public items, incompatible signatures or return types, new trait
bounds, changed error types, reduced visibility, removed features, newly required
feature gates, and increased minimum supported Rust versions. Additions can also
break callers: exhaustive enum variants, public struct fields used in literals,
and required methods on traits implemented downstream.

Record observable semantics, including validation, ordering, privacy, security,
and resource use.

New independent public types or functions and behavior-preserving optimizations
are not inherently breaking. Check trait implementations and method additions
for coherence or name-resolution conflicts. Adding an `Option<T>` parameter
still changes the signature; Rust has no default arguments.

### Dependency updates

A SemVer-incompatible dependency update breaks an API that exposes its types,
even if Zebra's signatures are textually unchanged. This includes `0.x` to
`0.(x+1)` updates. Use `breaking`, name the dependency and new version, identify
the exposed API, and explain the callers' required lockstep upgrade.

A compatible update needs a `Changed` entry for consumer-visible behavior.
Internal-only and development dependency changes need no fragment without such
an effect. Release-time dependency cascades can require a versioned entry for a
newly published package; audit the generated entry rather than treating it as
API evidence.

## Part 4: GitHub Release format

Use the release's generated changelog plus the operational information below.
Do not copy compatibility values or upgrade instructions from a previous release
without checking the actual source and artifacts.

### Update priority

Identify who must upgrade and why. Use a table when priorities differ by audience.

| Priority | Meaning |
| --- | --- |
| Critical | Immediate security response or a mandatory network-upgrade deadline |
| High | Breaking changes or consensus updates requiring prompt action |
| Medium | New features or notable fixes |
| Low | Minor improvements without an immediate operational requirement |

Include the network-upgrade deadline or security-release details specified above.

### How to upgrade

Give release-specific binary, source-build, and Docker instructions; link the
[upgrade procedure](../user/upgrading.md). Identify configuration migrations,
required backups, database upgrades, and downgrade restrictions before restart.
Do not invent service names or shutdown log messages.

### Compatibility

Record supported binary platforms and system-library requirements, active or
scheduled network upgrades, relevant RPC differences, and minimum Rust versions
for source builds. Read Rust requirements from that release's package manifests;
libraries and `zebrad` need not have the same requirement. Do not claim blanket
compatibility with every RPC in a `zcashd` version.

### Release structure

Use this order, omitting sections that do not apply:

1. Two-to-four-sentence summary and upgrade urgency.
2. Update-priority table when different audiences need different actions.
3. Network-upgrade details or security context, if applicable.
4. Changelog categories in the configured order.
5. Upgrade instructions and compatibility requirements.
6. Contributor and reporter credit where appropriate.

Do not include raw commit logs, contributor-only implementation details, or
repeated documentation. Link detailed procedures instead.

[changie-config]: https://github.com/ZcashFoundation/zebra/blob/main/.changie.yaml
[pr-gate]: https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/pr-gate.yml
[changelog-command]: https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/changelog-command.yml
[release-workflow]: https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/release.yml
[release-config]: https://github.com/ZcashFoundation/zebra/blob/main/.github/cargo-release.yml
[batch-script]: https://github.com/ZcashFoundation/zebra/blob/main/.github/scripts/batch-release-changelogs.sh
[security]: https://github.com/ZcashFoundation/zebra/blob/main/SECURITY.md
[cargo-semver]: https://doc.rust-lang.org/cargo/reference/semver.html

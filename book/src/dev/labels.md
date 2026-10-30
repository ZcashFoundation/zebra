# Labels and issue types

Issue types classify work; labels route review, hold merging, or request
automation. Labels below are current as of October 24, 2026. Inspect the live set
before changing workflow references:

```sh
gh label list --repo ZcashFoundation/zebra --limit 300
```

## Issue types replace type labels

Choose an issue type when filing an issue:

| Type | Use for |
| --- | --- |
| Bug | Incorrect behavior in Zebra, CI, documentation, or tooling |
| Task | Work without a new user-visible feature: maintenance, infrastructure, research |
| Feature | New user-visible functionality |
| Epic | A high-level goal broken into smaller issues |

Issue types apply to issues, not PRs. A PR declares its kind in its
[conventional-commit title](../CONTRIBUTING.md#pull-requests).

## Labels people apply

| Label | Purpose and rule |
| --- | --- |
| `security` | Security-relevant work of any severity. Report vulnerabilities privately through [SECURITY.md][security], not through a public issue. |
| `consensus` | Validation, cryptography, or script changes requiring consensus-aware review. |
| `devops` | Build, CI, test infrastructure, or release work. The devops issue template applies it automatically. |
| `urgent` | Needs attention today. It does not bypass checks or trigger special automation. |
| `blocked` | Waiting on an external dependency. Add a comment naming the blocker and a date to reassess it. |
| `do-not-merge` | Holds a PR even when checks pass. Resolve the hold with the person who applied it. |
| `needs-issue` | A PR lacks the issue discussion required by the contribution policy. Request or establish that discussion before proceeding. |
| `help wanted` | Maintainers invite outside help; prior discussion still applies. |
| `good first issue` | Suitable for a first contribution; keep this GitHub-native spelling. |
| `nu-7` | NU7 network-upgrade work. Remove the temporary label after the upgrade ships. |
| `ai-generated` | Existing AI-assisted backlog cohort. Do not apply it to new issues; remove it when the cohort is resolved. It is not a substitute for PR AI disclosure. |

The template named “Private Security Issue” creates a **public placeholder** for
maintainers tracking work discussed privately. It is not a private vulnerability
reporting channel. Never put exploit details in it.

## Labels wired into automation

| Label | Consumer or writer | Effect |
| --- | --- | --- |
| `do-not-merge` | `merge-policy.yml` | Fails the source PR's merge-policy check and checks every PR named in a merge group. |
| `external-contribution` | `label-external.yml` | Labels new issues and PRs by outside authors. Bots, owners, members, collaborators, and authors with admin, maintain, or write permission are excluded. |
| `release` | `release-plz.toml`, `release.yml`, `pr-gate.yml`, `tests-unit.yml`, `checkpoint-update.yml`, release templates | Identifies release work. Release readiness and publication also require a branch beginning with `release-plz-`; the label alone is insufficient. |
| `run-stateful-tests` | `zfnd-ci-integration-tests-gcp.yml` | Enables stateful integration runs for same-repository PRs. Fork PRs do not gain access to the privileged jobs by adding this label. |
| `run-benchmarks` | `benchmarks.yml` | Runs the PR benchmark comparison against its base while the label is present, including on subsequent PR updates. |

Labels do not override [workflow event filters and permissions][workflows].

## Labels written by CI

CI appends failures to the open issue with the matching label, or opens one.
Do not apply or repurpose these labels manually; close resolved failure issues.

| Label | Workflow |
| --- | --- |
| `ci-fail/advisory` | `advisory.yml`: scheduled advisory checks |
| `ci-fail/binaries` | `release-binaries.yml`: release artifact failures |
| `ci-fail/main` | `zfnd-ci-integration-tests-gcp.yml`: selected Mainnet and lightwalletd integration failures outside PRs; Testnet-only jobs are excluded |
| `ci-fail/release` | `zfnd-deploy-nodes-gcp.yml` and `zfnd-deploy-prebuilt-nodes-gcp.yml`: image-build and node-deployment failures |

## Other repository labels

| Label | Repository description |
| --- | --- |
| `dependencies` | PRs that update a dependency file |
| `github_actions` | PRs that update GitHub Actions code |
| `rust` | PRs that update Rust code |
| `wayfinder:map` | Wayfinder map of decision tickets |
| `wayfinder:research` | Wayfinder ticket resolved by research |
| `wayfinder:grilling` | Wayfinder ticket resolved by a decision conversation |
| `wayfinder:task` | Wayfinder ticket that unblocks a decision |

The [Dependabot configuration][dependabot] sets `labels: []` for both ecosystems;
it does not apply `dependencies`, `github_actions`, or `rust`. Wayfinder labels
track decisions, not Zebra merge or release gates.

## Adding or removing a label

Propose the decision a new label enables in a Team Decisions discussion. Use
kebab-case for new multi-word labels, except GitHub-native community labels.
Preserve existing names that integrations consume, including slash, underscore,
and colon forms.

Before renaming or deleting a label, search `.github/` and release configuration
and inspect saved project-view filters. Update all consumers in the same change:
missing labels can silently disable jobs, and saved filters need updating.

[security]: https://github.com/ZcashFoundation/zebra/blob/main/SECURITY.md
[workflows]: https://github.com/ZcashFoundation/zebra/tree/main/.github/workflows
[dependabot]: https://github.com/ZcashFoundation/zebra/blob/main/.github/dependabot.yml

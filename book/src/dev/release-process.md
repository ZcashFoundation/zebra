# Zebra versioning and releases

Zebra releases publish Rust crates, a `zebrad` GitHub Release, and downstream
artifacts. Separate planning, changelog, and publication steps let failed
publications resume without replacing immutable artifacts.

<a id="versioning"></a>

## Zebra versioning

Zebra uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html), with
independent versions for `zebrad` and each library crate. Judge compatibility
against each package's audience.

| Release level | `zebrad` | Library crates |
| --- | --- | --- |
| Major | Mainnet network-upgrade support or breaking operator interfaces, including configuration, CLI, RPC, and downgrade-incompatible state changes | Incompatible public APIs, semantics, features, or public dependency types |
| Minor | Backward-compatible features and behavior changes | Backward-compatible API additions |
| Patch | Compatible fixes without a new feature | Compatible fixes |

Follow [Cargo's SemVer rules][cargo-semver], including pre-1.0 versions, and the
[changelog guidelines](changelog-guidelines.md). A library major bump does not
itself require a `zebrad` major bump.

The [release-plz configuration][release-plz] selects releases from conventional
commits and dependency changes. Commit types `feat`, `fix`, `perf`, `refactor`,
and `build` trigger release consideration; dependency cascades can release other
packages. Release-plz disables changelog generation and SemVer checking;
maintainers must audit proposed versions and source-authored changelogs.

<a id="supported-releases"></a>

### Supported releases

Plan releases within the [Mainnet support window](../user/run.md#end-of-support).

A release must support its network's active upgrades regardless of its support
cutoff.

<a id="updating"></a>

### Supported update paths

Follow the [upgrade and rollback requirements](../user/upgrading.md).

<a id="previews"></a>

### Preview releases

A beta version (`X.Y.Z-beta.0`) identifies a development prerelease; a release
candidate (RC, `X.Y.Z-rc.0`) is selected for final testing. Package versions, tags,
and versioned changelogs must agree. Use the normal Release PR and readiness
process:

1. Agree on the candidate's scope and prerelease version. Check that the final
   Release PR manifests, dependency requirements, and versioned changelogs use
   that version; there is no separate `rc` dispatch operation in `release.yml`.
2. Review the readiness report's exact source commit and package plan, then
   approve publication through the normal release workflow.
3. Test the published candidate and record the results. Publish fixes with new
   versions; never replace the candidate's tag or crate archive.
4. Publish the stable version through a new reviewed version plan after testing.
   Do not turn an RC-version GitHub prerelease into a stable release by changing
   its channel. The controller derives the channel from the package's SemVer
   suffix and rejects a conflicting existing channel on recovery.

Publishing a GitHub prerelease does not deploy it to production. The
[GCP deployment workflow][deploy-workflow] and
[binary workflow][binary-workflow] handle `release: released` for
`v`-prefixed tags: stable publication or promotion of a prerelease.
Prereleases skip production deployment and stable Docker and binary
distribution. Manual dispatch can still target production independently
of the release channel.

### Distribution tags

Git tags use `v<version>` for `zebrad` and `<crate>-v<version>` for libraries.
For container tags, see [Docker images](../user/docker.md).

### Feature flags

Keep experimental features behind [Cargo features][cargo-features] so `main`
remains releasable. Agree on the treatment of breaking changes before merging;
a feature gate does not replace versioning or migration documentation. Security
fixes can require an immediate incompatible change.

<a id="frequency"></a>

## Release frequency

Publication is maintainer-controlled, not scheduled by a timer. Non-release
pushes to `main` refresh the Release PR; review and merge authorize publication.
Plan upgrades and release preparation before the current Mainnet support window
expires. Network-upgrade deadlines and urgent security fixes determine release
timing independently of routine feature and fix releases.

<a id="deprecation"></a>

## Deprecation practices

Announce deprecated operator interfaces in the changelog with replacements and
removal timelines. Retain them until the next major release, accepting only
critical and security fixes. If a critical security or reliability problem
requires earlier removal, explain why at the top of the release notes and
provide migration instructions.

Library APIs follow their crate's SemVer compatibility policy.

<a id="process"></a>
<a id="release-candidate--release-process"></a>

## Release candidate and release process

The [release workflow][release-workflow] maintains a `release-plz-` branch labeled
`release`. After review and merge, `ZcashFoundation/cargo-release` publishes it.
This action is not the local `cargo release` CLI configured by `release.toml`.

### Prepare and review the Release PR

Track the release with the [release issue template][release-issue]. Complete
preparation on `main` before the final Release PR refresh:

- Confirm a successful full sync since the last state change and the required
  integration evidence.
- Review and merge the checkpoint update.
- Set `ESTIMATED_RELEASE_HEIGHT` for the intended release. Check the support
  interval against upcoming upgrades: a release lacking an upgrade must expire
  before that upgrade; restore the intended interval in its supporting release.
- For network upgrades, check activation heights, consensus branch IDs, minimum
  peer protocol version, and supporting dependencies.
- Review outstanding security advisories and coordinate any private fixes under
  [SECURITY.md][security].

Review each package against its own previous published version. The
`PR Gate / release-api-diff` comment starts at the latest reachable `zebrad`
release tag and identifies crates with older release baselines. Review those
older ranges separately. Generated version bumps are not proof of correct SemVer.

Follow the [changelog batching and correction procedure](changelog-guidelines.md#release-batching-and-corrections).

Approve and merge only the latest reviewed commit after required checks pass.
A new Release PR commit invalidates review of the previous version plan. Approve
the `release` environment deployment when its protection rules require it.

### What release readiness reports

The `PR Gate / Release readiness` job requires current `main`, validates each
released package's versioned changelog, and runs Cargo 1.91's multi-package
publication dry run. Changelog and Cargo validation run independently and both
appear in the summary. The controller observes crates.io, Git tags, and the
GitHub Release without publishing.

Before publication, a successful report with `reason: "incomplete"` is expected:
the desired plan is valid, but the external artifacts do not all exist yet.
Review the plan and observed state, not only the job's green status.

Before first publication, have a crate owner confirm
[Trusted Publishing bindings](crate-owners.md#logging-in-to-cratesio).

### What happens after merge

The controller fixes the source to the Release PR's merge commit and the base to
its first parent. It publishes missing crates in dependency order and verifies
source provenance. When `zebrad` is in the plan, the workflow installs that exact
published version with `cargo install --locked --version`, runs it with
`--version`, and only then creates missing tags and the GitHub Release.

Release notes come from the [generated changelog](changelog-guidelines.md#changelogmd-vs-github-release).
For downstream jobs, see [continuous delivery](continuous-delivery.md); for
artifacts and signatures, see [binary verification](../user/install.md#pre-built-binaries).

Confirm publication, binary distribution, and production health before
announcing success. Then run the
[Book workflow](https://github.com/ZcashFoundation/zebra/actions/workflows/book.yml)
manually, so the site root documents the new release without waiting for the
next push to `main`; if `BOOK_ROOT_REF` is set, delete it first.

### If release readiness fails

Read the failed job summary before retrying.

| Failure | Action |
| --- | --- |
| Release PR is behind `main` | Let release-plz refresh it, then review the new head. |
| Missing or empty versioned changelog | Add missing source fragments on `main` for direct changes. A dependency-only omission requires a batching fix, not a generated-file patch. |
| Cargo dry run fails | Fix the source or dependency problem on `main`. |
| Conflicting crate provenance, tag target, or release channel | Stop publication and investigate; do not overwrite immutable external state. |

If readiness does not start, rerun the PR checks. For manual fallback, set
`PR_NUMBER` to the open Release PR:

```sh
gh workflow run release.yml --repo ZcashFoundation/zebra --ref main \
  -f operation=check -f release_pr_number="${PR_NUMBER}"
```

### If publication stops after merge

Read the failure summary. Set `PR_NUMBER` to the same merged Release PR:

```sh
gh workflow run release.yml --repo ZcashFoundation/zebra --ref main \
  -f operation=resume -f release_pr_number="${PR_NUMBER}"
```

The `resume` operation keeps the immutable source, skips matching crates and
tags, and fills missing external state. It repairs configured mutable metadata
(name, notes, draft state), but rejects conflicting crate sources, tag targets,
or prerelease channels.

If automation remains unavailable, obtain maintainer authorization to use the
[legacy checklist][legacy-checklist]. Never run manual publication concurrently
with the controller.

### Withdraw a bad release

Obtain maintainer approval and select versions for withdrawal only for consensus
failures, data corruption, security vulnerabilities, or sync regressions that
stall nodes. Fix other defects through a regular release.

Set `BAD` to the withdrawn version and `GOOD` to the proposed rollback version,
both without `v`. The rollback target must lack the defect, remain supported on
Mainnet, and support upgrades active or activating before the fix ships on each
affected network. Compare its `zebra-chain/src/parameters/constants.rs` with the
network's activation heights. If no safe rollback exists, ship the fix without
redirecting Latest, Docker `latest`, or production to an obsolete release.

Steps 1–4 can run in the order that limits harm fastest. Start step 5 when the
fix is ready; step 6 waits for fixed crate versions to be published.

1. **Warn on the GitHub Release and remove installable assets.** Prepend a
   warning naming affected versions, the defect, and any replacement; retain
   the original notes:

   ```sh
   gh release view "v${BAD}" --repo ZcashFoundation/zebra \
     --json body --jq .body > notes.md
   # Edit notes.md to prepend the withdrawal warning.
   gh release edit "v${BAD}" --repo ZcashFoundation/zebra \
     --notes-file notes.md
   ```

   If the withdrawn release is Latest and a safe rollback exists, select it:

   ```sh
   gh release edit "v${GOOD}" --repo ZcashFoundation/zebra --latest
   ```

   Download assets needed for analysis before deleting them:

   ```sh
   gh release view "v${BAD}" --repo ZcashFoundation/zebra \
     --json assets --jq '.assets[].name' |
     while IFS= read -r asset; do
       gh release delete-asset "v${BAD}" "${asset}" \
         --repo ZcashFoundation/zebra --yes
     done
   ```

   Keep the release and Git tag as the record of what shipped. Do not change its
   prerelease/stable channel or convert it to a draft: channel changes conflict
   with recovery, and promotion restarts distribution jobs. Do not run `resume`
   for the withdrawn PR; it can restore original notes and release metadata.

2. **Remove the Docker installation path.** Compare the current `latest` and
   withdrawn image digests before changing `latest`:

   ```sh
   docker buildx imagetools inspect --format '{{.Manifest.Digest}}' \
     zfnd/zebra:latest
   docker buildx imagetools inspect --format '{{.Manifest.Digest}}' \
     "zfnd/zebra:${BAD}"
   ```

   Only when those digests match and a safe rollback exists, copy the good
   release's image index:

   ```sh
   docker buildx imagetools create -t zfnd/zebra:latest "zfnd/zebra:${GOOD}"
   docker buildx imagetools inspect --format '{{.Manifest.Digest}}' \
     zfnd/zebra:latest
   docker buildx imagetools inspect --format '{{.Manifest.Digest}}' \
     "zfnd/zebra:${GOOD}"
   ```

   Require matching digests. Copying the image index preserves its digest-bound
   signatures and attestations. Delete the withdrawn Docker Hub version tag and
   matching `sha-<short-commit>` tag; remove only aliases with the withdrawn
   digest. Pulls by digest can still succeed, and downloaded copies remain usable.

3. **Roll production back only where safe.** Check disk compatibility and inspect
   the good tag's deployment workflow: the tag supplies both code and workflow.
   For compatible state, select one supported network and zone:

   ```sh
   NETWORK=Mainnet
   ZONE=us-east1-b
   gh workflow run zfnd-deploy-nodes-gcp.yml \
     --repo ZcashFoundation/zebra --ref "v${GOOD}" \
     -f network="${NETWORK}" -f zone="${ZONE}" \
     -f environment=prod -f need_cached_disk=false
   ```

   Manual dispatch waits for rollout, not node-health verification. Check the
   selected managed instance group (MIG) using
   [deployment diagnostics](gcp-deployment-operations.md#diagnose-a-stuck-mig),
   with `P=zfnd-prod-zebra` and its production name, such as
   `MIG=zebrad-mainnet-b`. Verify health before repeating for another zone or
   network; dispatching all six jobs together does not serialize rollouts.

   If the old binary cannot use the disk format, follow the
   [database-format rollback procedure](gcp-deployment-operations.md#db-format-version-break-release).
   Confirm a pre-upgrade snapshot before destructive disk operations; snapshots
   named `-pre-major-<timestamp>` exist only if preparation created them. Choose
   the procedure by migration and rollback compatibility, not major version.

4. **Notify operators.** Keep the GitHub Release warning as the primary notice.
   For a vulnerability in a tagged release, publish a GitHub Security Advisory
   naming affected and fixed versions under [the disclosure policy][security].
   Coordinate disclosure timing rather than exposing exploit details prematurely.

5. **Publish the fix.** Use the normal release process and a `changie new`
   fragment naming the withdrawn versions and the fix. Never reuse a published
   version number.

6. **Yank defective crate versions after publishing the fix.** Always yank the
   affected `zebrad` version; yank libraries only if their code carries the defect.
   Use the publication plan's exact crate versions and a token restricted to
   yanking; follow the [credential procedure](crate-owners.md#logging-in-to-cratesio):

   ```sh
   cargo yank zebrad --version "${BAD}"
   ```

   A yank prevents new dependency resolution from selecting that version;
   existing `Cargo.lock` files remain usable. It does not uninstall affected
   binaries or revoke existing source archives.

[cargo-semver]: https://doc.rust-lang.org/cargo/reference/semver.html
[cargo-features]: https://doc.rust-lang.org/cargo/reference/features.html
[release-plz]: https://github.com/ZcashFoundation/zebra/blob/main/release-plz.toml
[release-workflow]: https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/release.yml
[release-issue]: https://github.com/ZcashFoundation/zebra/issues/new?template=release.md
[binary-workflow]: https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/release-binaries.yml
[deploy-workflow]: https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zfnd-deploy-nodes-gcp.yml
[security]: https://github.com/ZcashFoundation/zebra/blob/main/SECURITY.md
[legacy-checklist]: https://github.com/ZcashFoundation/zebra/blob/main/.github/PULL_REQUEST_TEMPLATE/release-checklist-legacy.md

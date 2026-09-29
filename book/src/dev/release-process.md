# Zebra versioning and releases

This document contains the practices that we follow to provide you with a leading-edge application, balanced with stability.
We strive to ensure that future changes are always introduced in a predictable way.
We want everyone who depends on Zebra to know when and how new features are added, and to be well-prepared when obsolete ones are removed.

Before reading, you should understand [Semantic Versioning](https://semver.org/spec/v2.0.0.html) and how a [Trunk-based development](https://www.atlassian.com/continuous-delivery/continuous-integration/trunk-based-development) works

<a id="versioning"></a>

## Zebra versioning

Zebra version numbers show the impact of the changes in a release. They are composed of three parts: `major.minor.patch`.
For example, version `3.1.11` indicates major version 3, minor version 1, and patch level 11.

The version number is incremented based on the level of change included in the release.

| Level of change | Details                                                                                                                                                                                                                                                              |
| :-------------- | :------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Major release   | Contains significant new features, and commonly correspond to network upgrades; some technical assistance may be needed during the update. When updating to a major release, you may need to follow the specific upgrade instructions provided in the release notes. |
| Minor release   | Contains new smaller features. Minor releases should be fully backward-compatible. No technical assistance is expected during update. If you want to use the new features in a minor release, you might need to follow the instructions in the release notes.        |
| Patch release   | Low risk, bug fix release. No technical assistance is expected during update.                                                                                                                                                                                        |

<a id="supported-releases"></a>

### Supported Releases

Every Zebra version released by the Zcash Foundation is supported up to a specific height. Currently we support each version for about **12 weeks** (see `EOS_PANIC_AFTER` in `zebrad/src/components/sync/end_of_support.rs`) but this can change from release to release.

When the Zcash chain reaches this end of support height, `zebrad` will shut down and the binary will refuse to start.

Our process is similar to `zcashd`: <https://zcash.github.io/zcash/user/release-support.html>

Older Zebra versions that only support previous network upgrades will never be supported, because they are operating on an unsupported Zcash chain fork.

<a id="updating"></a>

### Supported update paths

You can update to any version of Zebra, provided that the following criteria are met:

- The version you want to update _to_ is supported.
- The version you want to update _from_ is within one major version of the version you want to upgrade to.

<a id="previews"></a>

### Preview releases

We let you preview what's coming by providing Release Candidate \(`rc`\) pre-releases for some major releases:

| Pre-release type  | Details                                                                                                                                                                |
| :---------------- | :--------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Beta              | The release that is under active development and testing. The beta release is indicated by a release tag appended with the `-beta` identifier, such as `8.1.0-beta.0`. |
| Release candidate | A release for final testing of new features. A release candidate is indicated by a release tag appended with the `-rc` identifier, such as version `8.1.0-rc.0`.       |

### Distribution tags

Zebra's tagging relates directly to versions published on Docker. We will reference these [Docker Hub distribution tags](https://hub.docker.com/r/zfnd/zebra/tags) throughout:

| Tag    | Description                                                                                         |
| :----- | :-------------------------------------------------------------------------------------------------- |
| latest | The most recent stable version.                                                                     |
| beta   | The most recent pre-release version of Zebra for testing. May not always exist.                     |
| rc     | The most recent release candidate of Zebra, meant to become a stable version. May not always exist. |

### Feature Flags

To keep the `main` branch in a releasable state, experimental features must be gated behind a [Rust feature flag](https://doc.rust-lang.org/cargo/reference/features.html).
Breaking changes should also be gated behind a feature flag, unless the team decides they are urgent.
(For example, security fixes which also break backwards compatibility.)

<a id="frequency"></a>

## Release frequency

We work toward a regular schedule of releases, so that you can plan and coordinate your updates with the continuing evolution of Zebra.

<div class="alert is-helpful">

Dates are offered as general guidance and are subject to change.

</div>

In general, expect the following release cycle:

- A major release for each network upgrade, whenever there are breaking changes to Zebra (by API, severe bugs or other kind of upgrades)
- Minor releases for significant new Zebra features or severe bug fixes
- A patch release around every 6 weeks

This cadence of releases gives eager developers access to new features as soon as they are fully developed and pass through our code review and integration testing processes, while maintaining the stability and reliability of the platform for production users that prefer to receive features after they have been validated by Zcash and other developers that use the pre-release builds.

<a id="deprecation"></a>

## Deprecation practices

Sometimes "breaking changes", such as the removal of support for RPCs, APIs, and features, are necessary to:

- add new Zebra features,
- improve Zebra performance or reliability,
- stay current with changing dependencies, or
- implement changes in the \(blockchain\) itself.

To make these transitions as straightforward as possible, we make these commitments to you:

- We work hard to minimize the number of breaking changes and to provide migration tools, when possible
- We follow the deprecation policy described here, so you have time to update your applications to the latest Zebra binaries, RPCs and APIs
- If a feature has critical security or reliability issues, and we need to remove it as soon as possible, we will explain why at the top of the release notes

To help ensure that you have sufficient time and a clear path to update, this is our deprecation policy:

| Deprecation stages | Details                                                                                                                                                                                                                                                                                                                                                                                |
| :----------------- | :------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Announcement       | We announce deprecated RPCs and features in the [change log](https://github.com/ZcashFoundation/zebra/blob/main/CHANGELOG.md "Zebra change log"). When we announce a deprecation, we also announce a recommended update path.                                                                                                                                                          |
| Deprecation period | When a RPC or a feature is deprecated, it is still present until the next major release. A deprecation can be announced in any release, but the removal of a deprecated RPC or feature happens only in major release. Until a deprecated RPC or feature is removed, it is maintained according to the Tier 1 support policy, meaning that only critical and security issues are fixed. |
| Rust APIs          | The Rust APIs of the Zebra crates are currently unstable and unsupported. Use the `zebrad` commands or JSON-RPCs to interact with Zebra.                                                                                                                                                                                                                                               |

<a id="process"></a>

## Release candidate & release process

The normal release path requires 2 maintainer actions:

1. Review the latest Release PR after every required check passes.
2. Approve and merge the latest commit.

Everything else is automatic. release-plz creates and updates a PR whose branch starts with `release-plz-` and carries the `release` label, `PR Gate / Release readiness` validates it, and `ZcashFoundation/cargo-release` publishes from that PR's source range after merge.

### Review the Release PR

Wait until release-plz finishes updating the PR and every required check passes, then review the latest commit and complete every checkbox in its generated checklist. Each checked box records that a maintainer performed that validation; for a conditional item, check it after validating the condition or confirming that it does not apply. Checklist edits use the standard PR Gate workflow, so wait for the latest run before approval. Source PRs commit curated change fragments under `.changes/unreleased/`, then the Release workflow batches them into versioned entries for the versions release-plz picked and regenerates every changelog, writing a mechanical dependency entry for a package that is being released only because a local dependency moved. Before approval, any required checkpoint, end-of-support height, README, or operational release-note changes must land on `main`.

A new Release PR commit replaces the generated body and resets every checkbox. Treat only the latest checklist and required-check results as authoritative.

Approve and merge only after every required check passes and every checkbox is complete. A later release-plz update invalidates the earlier review and checklist.

### What Release Readiness Reports

Every new Release PR commit automatically runs `PR Gate / Release readiness`. The job confirms that the PR includes current `main`, validates each changed package's versioned changelog, and runs Cargo 1.91's multi-package dry-run. Changelog and Cargo validation run independently, so the summary reports both outcomes even when one fails. The job also observes crates.io, tags, and the GitHub Release without changing them.

Before publication, a green report with `reason: "incomplete"` is expected: the plan and dry-run passed, while the planned crates, tags, or GitHub Release are correctly absent. The job summary shows the complete plan and observed state, so maintainers can review readiness without running local commands.

### What Happens After Merge

Merging the approved Release PR starts the post-merge release workflow. The controller uses the merge commit as the immutable release source and its first parent as the publication range base. Cargo Release publishes missing crates in dependency order, verifies that the published `zebrad` installs when it is part of the plan, then creates missing tags and the public `zebrad` GitHub Release. The release commit does not create another Release PR. That GitHub Release triggers the downstream workflows that publish signed Docker images, attach signed and checksummed Linux `x86_64` and `aarch64` binaries, and deploy long-lived GCP nodes.

No maintainer command is required when this workflow succeeds.

> [!IMPORTANT]
> Before the first release using this controller, a crates.io owner must confirm [Trusted Publishing](https://crates.io/docs/trusted-publishing) for every publishable workspace crate with repository `ZcashFoundation/zebra`, workflow `release.yml`, and environment `release`. Trusted Publisher bindings are visible only to crate owners, so repository CI cannot verify this prerequisite.

### If Release Readiness Fails

Open the failed job summary before retrying. Each failure identifies the next action:

| Failure | Next action |
| --- | --- |
| The Release PR is behind `main` | Wait for release-plz to update the PR. |
| A versioned changelog is missing or empty | For a direct package change, add the missing fragment on `main` with `changie new -j <project>`, then let release-plz refresh the PR. A dependency-only failure indicates a changelog batching regression; do not edit the generated branch. |
| Cargo's dry-run fails | Fix the source or dependency problem on `main`. |
| Crate provenance, a tag target, or a release channel conflicts | Stop and ask a maintainer to investigate. |

Each new Release PR commit reruns the complete readiness check.

If no readiness run is available, first rerun the PR checks in GitHub. The manual dispatch below is the readiness exit hatch when the automatic PR check still does not start:

```sh
gh workflow run release.yml --ref main \
  -f operation=check \
  -f release_pr_number=<PR>
```

### If Publication Stops After Merge

Resume the same merged Release PR:

```sh
gh workflow run release.yml --ref main \
  -f operation=resume \
  -f release_pr_number=<PR>
```

`resume` reuses the same Release PR source, skips matching crates and tags, and continues from missing external state. It can repair configured mutable GitHub Release metadata, such as the release name, notes, draft state, or latest selection.

Do not retry immutable contradictions: a crate archive from another source commit, a tag that points to another commit, or a GitHub Release with a conflicting channel must stop for maintainer review. Do not manually repeat publication or overwrite external state.

If the automated workflow remains unavailable and a maintainer authorizes a break-glass manual release, follow the [manual release checklist](https://github.com/ZcashFoundation/zebra/blob/main/.github/PULL_REQUEST_TEMPLATE/release-checklist-legacy.md).

### Withdraw a Bad Release

Withdraw a release when it has a defect that operators must not install or keep running, such as a consensus failure, data corruption, a security vulnerability, or a sync regression that stalls nodes. Fix any other defect in a regular release. A maintainer makes the call, then works through these steps for each affected version. Steps 1 to 4 can run in any order; start with the step that limits harm fastest. Start step 5 as soon as a fix is ready; step 6 waits for the release it publishes.

In the commands, `<bad>` is the withdrawn version and `<good>` is the rollback target, both without the `v` prefix. `<good>` is the newest release that lacks the defect, has not reached its end-of-support height (see [Supported Releases](#supported-releases)), and supports every network upgrade that is active, or activates before the fix ships, on each network being rolled back. Compare the activation heights in `zebra-chain/src/parameters/constants.rs` at `v<good>` with the chain. A node on a release that lacks an active upgrade follows the old-rules chain, and the fixed release stops at startup on that state with "Cached state contains a legacy chain". If no release qualifies, do not point Latest, `latest`, or production at an older release; ship the fix.

1. Edit the GitHub Release. Put a warning block at the top of the notes that names the withdrawn versions and the version to install, if there is one, and keep the rest of the notes:

   ```sh
   gh release view v<bad> --json body --jq .body > notes.md
   # Add a "> [!WARNING]" block at the top of notes.md
   gh release edit v<bad> --notes-file notes.md
   ```

   If the withdrawn release is marked Latest, mark `v<good>` instead with `gh release edit v<good> --latest`. Delete the binaries so nobody installs them, after downloading any that the analysis needs:

   ```sh
   gh release view v<bad> --json assets --jq '.assets[].name' | xargs -I{} gh release delete-asset v<bad> {} --yes
   ```

   Keep the release and its git tag as the record of what shipped. Leave it a full release, not a pre-release or draft: `resume` treats a changed channel as a conflict, and turning a pre-release back into a release runs `release-binaries.yml` again. Do not run `resume` for the withdrawn version's Release PR, because it can restore the planned release notes and Latest selection.

2. Fix the Docker Hub tags. If `zfnd/zebra:latest` has the digest of the withdrawn image, point it at `<good>`. The copy keeps the image digest, so signatures and attestations still verify:

   ```sh
   docker buildx imagetools inspect --format '{{.Manifest.Digest}}' zfnd/zebra:latest
   docker buildx imagetools inspect --format '{{.Manifest.Digest}}' zfnd/zebra:<bad>
   docker buildx imagetools create -t zfnd/zebra:latest zfnd/zebra:<good>
   ```

   Run the `create` command only when the first two digests match, then check that `zfnd/zebra:latest` and `zfnd/zebra:<good>` print the same digest. Delete the `<bad>` and `sha-<7 chars>` tags of the withdrawn image in the Docker Hub repository, so nobody can fetch it by tag. A pull by digest can still succeed.

3. Roll production back to `<good>`. `zfnd-deploy-nodes-gcp.yml` builds and deploys the commit of the ref it runs from, so dispatch it from the good release tag, once per network and zone. Leave out a network whose active upgrades `<good>` does not support. The production state disks already exist, so skip the cached-disk lookup:

   ```sh
   for network in Mainnet Testnet; do
     for zone in us-east1-b us-east1-c us-east1-d; do
       gh workflow run zfnd-deploy-nodes-gcp.yml --ref v<good> \
         -f network="${network}" -f zone="${zone}" \
         -f environment=prod -f need_cached_disk=false
     done
   done
   ```

   A dispatch waits for the new template to roll out, not for node health, so check each MIG with the commands in [GCP Deployment Operations](gcp-deployment-operations.md#diagnose-a-stuck-mig), using `P=zfnd-prod-zebra` and the production names `zebrad-<network>-<zone letter>`. If the withdrawn release changed the major `DATABASE_FORMAT_VERSION` in `zebra-state/src/constants.rs`, `<good>` cannot use the upgraded disks. The [rollback for that case](gcp-deployment-operations.md#db-format-version-break-release) deletes each disk and recreates it from its `-pre-major-<timestamp>` snapshot, so confirm the snapshots exist first; they exist only if that section's snapshot loop ran before the release.

4. Tell operators. The warning on the GitHub Release is the primary notice. If the defect is a vulnerability, publish a GitHub Security Advisory that names the affected and fixed versions, as described in [SECURITY.md](https://github.com/ZcashFoundation/zebra/blob/main/SECURITY.md#advisories-cves-and-credit).

5. Ship the fix through the normal process, with a change fragment (`changie new`) that names the withdrawn versions. Version numbers are never reused.

6. Yank the crate versions that carry the defect, only after step 5 has published the fixed versions. release-plz ignores yanked versions, so yanking the version that `main` still carries breaks the fix's Release PR. Always yank `zebrad`; yank a library crate only if its own code has the defect. The plan in the job summary of the release run's `Publish crates and GitHub Release` job lists every published crate version. Use a crates.io token limited to yanking (see [Zebra crates](crate-owners.md#logging-in-to-cratesio)):

   ```sh
   cargo yank zebrad@<bad>
   ```

   Existing `Cargo.lock` files keep working.

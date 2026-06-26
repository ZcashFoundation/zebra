---
status: accepted
date: 2026-06-26
builds-on: [Reproducible and signed release images](0007-reproducible-builds.md)
story: Which artifacts the binary release should ship
---

# Release binary artifacts and supply-chain posture

## Context and Problem Statement

The release pipeline publishes downloadable `zebrad` binaries alongside the signed release images covered in [0007](0007-reproducible-builds.md). Each release attaches a fixed, small set of files.

Comparable Zcash and Rust node projects attach far more per release: Debian packages backed by an APT repository, per-artifact PGP signatures, attached provenance bundles, and per-artifact SBOMs. Each adds cost in build complexity, signing-key custody, or ongoing operation. The open question for a validator node is which of them add enough value to justify that cost.

## Priorities & Constraints

- `zebrad` is a validator node, run by operators and stakers, mostly on servers. It is not a desktop wallet.
- The binary is statically linked: RocksDB and the zcash FFI are linked in, so the only runtime dependency is glibc >= 2.34, the documented floor.
- One build system and one signing model. Reuse the native Ubuntu matrix and Sigstore keyless signing already established for the image path; avoid a second build system and avoid a long-lived key.
- Every shipped artifact is both a security signal and a support surface. An artifact nobody verifies is pure cost, and one that misrepresents what it covers is worse than shipping nothing.
- Release images already carry their own signed SLSA provenance and SBOM ([0007](0007-reproducible-builds.md)).

## Current artifacts

Per release, for `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`:

- `zebrad-<version>-<target>.tar.gz` (the binary plus `LICENSE-APACHE`, `LICENSE-MIT`, `README.md`) and a `.tar.gz.sha256` sidecar.
- One `SHA256SUMS` manifest covering both archives.
- `SHA256SUMS.sigstore.json`: a Cosign keyless (Sigstore) signature over the manifest, with the Fulcio certificate identity pinned to the release workflow.
- SLSA v1 build provenance per archive via `actions/attest-build-provenance`, stored in GitHub's attestation API and verified with `gh attestation verify --signer-workflow`.

Six assets. Integrity and provenance for the whole release rest on one signed manifest plus one attestation per archive, not on per-file signatures. `cargo binstall zebrad` consumes these archives, giving a package-manager-style one-liner with no hosted repository to operate.

## Considered Options

Additional artifacts and mechanisms observed in peer node and wallet releases:

1. Debian `.deb` packages.
2. A hosted, signed APT repository.
3. Per-artifact PGP/GPG `.asc` detached signatures over a maintainer-held key.
4. Attached SLSA provenance files (`.intoto.jsonl` and a hand-rolled `.provenance.json`).
5. Per-artifact SBOM files (`.sbom.spdx`).
6. Raw, un-archived binaries instead of `.tar.gz`.
7. A second per-architecture build system (for example Nix for one arch, a from-source bootstrap for the other).

## Decision Outcome

Keep the six-asset posture above. The seven options are rejected for the binary release, each with the condition that would reopen it.

**1. Debian `.deb` packages: rejected.** A `.deb`'s headline value is dependency resolution. A statically linked `zebrad` depends only on glibc >= 2.34, which the archive already documents, so apt resolves nothing the operator lacks. A `.deb` merely attached to a release delivers no apt user experience: it is `dpkg -i` with extra packaging cost, no dependency resolution and no update tracking. Peer validators (Bitcoin Core, reth, Lighthouse) ship archive plus image plus source and no `.deb`. _Revisit if_ the project commits to a fleet-style channel or to APT continuity for the zcashd-to-Zebra migration; even then the package is the cheap part and the repository is the cost (see option 2).

**2. Hosted APT repository: rejected.** This is the only thing that delivers a real apt experience, and it is a permanent operations commitment, not a per-release step: GPG key custody and rotation, signing `Release`/`InRelease`/`Packages` for every supported distro codename, hosting the pool, and CI to re-index and re-sign on every publish. For a consensus node it is worse than neutral. Auto-updating a validator onto a new binary mid-consensus is an anti-feature operators avoid, and the end-of-support-height halt is incompatible with Debian-stable version pinning that freezes for years. _Revisit_ only alongside an explicit decision to operate such a channel.

**3. Per-artifact PGP signatures: rejected.** The release already signs a single `SHA256SUMS` manifest with Cosign keyless, recorded in Rekor with the signer identity pinned to the workflow. PGP reintroduces a long-lived organization-held key to custody, rotate, revoke, and expose on the runner, which for a consensus-critical node is a net security regression: keyless has no secret to leak. The one niche PGP fills is a distro packager pinning a maintainer key. _Revisit if_ a concrete downstream packager requires OpenPGP, and then add an `.asc` alongside Cosign rather than switching.

**4. Attached provenance files: rejected.** The release already mints the SLSA predicate with `actions/attest-build-provenance` and stores it in the attestation API; `gh attestation download` materializes the identical signed bundle on demand, and reproducible builds give a stronger source-to-binary check than any predicate. Attaching the `.intoto.jsonl` only saves one download command. The hand-rolled `.provenance.json` summary is unsigned, so it is unsafe to trust on its own and attaching it adds a misleading file. _Revisit if_ an explicit air-gapped or zero-GitHub-contact verification requirement appears, and then attach only the signed `.intoto.jsonl`, never an unsigned summary.

**5. Per-artifact SBOM files: rejected as commonly shipped.** A post-hoc SBOM of the stripped, static binary carries no dependency graph (Syft yields an empty catalogue) and merely restates a checksum, so it delivers no scanner or compliance value while adding a security signal that must be kept accurate. A crate-graph SBOM from `Cargo.lock` is cheaper to produce, but on its own it misrepresents the binary: it gives the statically linked native C/C++ libraries no scanner-matchable version and lists crates the binary never links. A useful SBOM is a different, native-corrected artifact (see Gated future work). _Revisit_ per that entry.

**6. Raw, un-archived binaries: rejected.** Shipping the bare executable saves one extraction step at the cost of the dual-license attribution that travels in the archive, the guaranteed executable bit, and a clean standalone-checksum story. The archive-with-licenses layout is the idiomatic Rust release convention.

**7. A second per-architecture build system: rejected.** Both targets build on native Ubuntu runners with one toolchain, reproducibly ([0007](0007-reproducible-builds.md)). A second build system is the dominant complexity driver in pipelines that adopt it and produces no artifact an end user can distinguish. It also invites build-system asymmetries (for example an unstripped binary on one arch), which are a regression, not richer content.

### Expected Consequences

- The binary release stays at six assets. Its integrity and provenance posture is a superset of a per-file-signed layout: one signed manifest plus reproducibility plus a stored attestation, rather than many individually signed files with no manifest.
- One keyless signing model across binaries and images. No long-lived key to custody.
- Each rejection has a recorded revisit trigger, so the artifact set changes only when a trigger is met.

### Gated future work

Two items wait on a concrete trigger.

#### Native-corrected crate SBOM

Generate it in the matrix build job with `cargo-sbom` (SPDX 2.3 JSON, matching the image SBOM) or `cargo-cyclonedx` (CycloneDX JSON), resolved for the release target, and attest it with `actions/attest-sbom` beside the existing build provenance. The build job already grants `id-token: write` and `attestations: write`, so the plumbing is two steps with no permission change.

The plumbing is not the cost. A `Cargo.lock` SBOM both over- and under-reports this binary, so it has to be corrected first:

- It lists `bzip2-sys`, `libz-sys`, and `aws-lc-sys` as if linked, when none are. `bzip2` is never compiled (`zebra-state` pins `rocksdb` with `default-features = false, features = ["lz4"]`), `libz-sys` is build-time-only through `vergen-git2`, and `ring` is the TLS backend.
- It gives the five linked native C/C++ libraries no scanner-matchable component. Each needs a `pkg:generic` entry with a pinned version and CPE: RocksDB 8.10.0 (`librocksdb-sys`), lz4 1.10.0 (`lz4-sys`), libsecp256k1 (`secp256k1-sys`, plus a second copy vendored in `libzcash_script`), the zcashd consensus C++ in `libzcash_script` 0.1.0, and the BoringSSL fork in `ring`.

That augmentation has to be re-validated on every bump of those crates, and the recurring work is the real cost. Gate it on a concrete enterprise, federal, or Cyber Resilience Act consumer. Until then `Cargo.lock` is public and builds are reproducible, so any consumer can regenerate the crate graph. Consumers verify with:

```sh
gh attestation verify <archive> \
  --repo ZcashFoundation/zebra \
  --signer-workflow ZcashFoundation/zebra/.github/workflows/zfnd-release-binaries.yml \
  --predicate-type https://spdx.dev/Document/v2.3
```

A cheaper step fixes only the over-reporting, with no gate: build with `cargo-auditable` so the embedded, feature-accurate dependency list is readable by `syft`, `cargo-audit`, and `trivy`.

#### systemd unit and service user

A documented systemd unit file and service-user snippet in the install docs are the low-cost substitute for the one genuine `.deb` convenience: daemon lifecycle and a dedicated account, with none of the packaging or repository machinery. Ship if bare-metal operators ask.

## More Information

- SLSA v1.0: <https://slsa.dev/spec/v1.0/levels>
- Sigstore Cosign keyless signing: <https://docs.sigstore.dev/cosign/signing/overview/>
- GitHub artifact attestations: <https://docs.github.com/actions/security-for-github-actions/using-artifact-attestations/using-artifact-attestations-to-establish-provenance-for-builds>
- `cargo-deb`: <https://github.com/kornelski/cargo-deb>
- `cargo-cyclonedx`: <https://github.com/CycloneDX/cyclonedx-rust-cargo>
- `cargo-sbom`: <https://github.com/psastras/sbom-rs>
- `actions/attest-sbom`: <https://github.com/actions/attest-sbom>
- `cargo-auditable`: <https://github.com/rust-secure-code/cargo-auditable>
- `cargo binstall`: <https://github.com/cargo-bins/cargo-binstall>

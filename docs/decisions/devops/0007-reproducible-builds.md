---
status: proposed
date: 2026-06-24
builds-on: [Docker high UID](0001-docker-high-uid.md)
story: Reproducible builds and supply-chain provenance for zebrad release images
---

# Reproducible Docker builds and supply-chain provenance posture

## Context and Problem Statement

We want independent parties to rebuild a published `zebrad` from source and get
the same bytes, and we want the build platform itself to be harder to tamper
with. These are two separate goals that are easy to conflate.

SLSA Build Level 3 does **not** require reproducible builds. L3 requires an
isolated build environment and non-falsifiable provenance; reproducibility was
the deferred Level 4 concept and is absent from SLSA v1.0. So "reproducible
builds for SLSA 3" is two tracks:

- **Provenance / SLSA level**: trust the artifact because of who and how it was
  built (isolation + signing).
- **Reproducibility**: trust the artifact because anyone can rebuild it from
  source and match the bytes.

Zebra already emits `provenance: mode=max` + SBOM with OIDC, builds with
`--locked`, sets `CARGO_INCREMENTAL=0`, pins apt package versions, and avoids
baking a wall-clock build timestamp into the binary. It was not yet
bit-for-bit reproducible, and the release path had provenance gaps.

## Priorities & Constraints

- Reuse the existing multi-stage Dockerfile, GitHub Actions, and Docker Build
  Cloud setup. No new build system (no Guix, no srtool).
- The shippable artifact is the native `zebrad` binary and its OCI image, not a
  WASM/BPF blob, so ecosystem tools like srtool and solana-verify do not apply
  directly; only their discipline does.
- Keep the build hermetic and inputs explicit.

## Considered Options

The main open question was how the binary records its source commit:

- Option A: keep `.git` out of the build context; pass the commit as the
  `SHORT_SHA` build arg.
- Option B: include `.git` in the build context (`!.git` in `.dockerignore`)
  and let `vergen` populate `VERGEN_GIT_*` from the repository.

### Pros and Cons of the Options

#### Option A: `.git` excluded, commit via `SHORT_SHA`

- Good, because the build is a pure function of the source tree plus explicit
  build args. An external rebuilder needs the source and the documented
  `SHORT_SHA`, not a matching clone.
- Good, because it avoids clone-shape nondeterminism: `VERGEN_GIT_BRANCH` differs
  on a detached-HEAD tag checkout versus a branch, and `git describe` depends on
  clone depth and whether tags were fetched. Either would make the same commit
  compile to different bytes depending on how it was checked out.
- Good, because the build context stays small.
- Bad, because the binary carries less embedded git metadata (no
  `git describe`). The release version still comes from `CARGO_PKG_VERSION`,
  which is correct for a tagged release.

#### Option B: `.git` included, metadata via `vergen`

- Good, because the binary self-describes precisely (`git describe`, branch,
  commit timestamp).
- Bad, because that metadata depends on the clone shape, which directly breaks
  reproducibility, the property we are trying to establish.
- Bad, because it ships the full history into the build context.

## Decision Outcome

Chosen option: **Option A**. Keep `.git` out of the build context and record the
commit through the `SHORT_SHA` build arg. `.dockerignore` documents this, and the
release workflow passes `short_sha: auto` so released binaries carry their commit.

Alongside that, to make the build reproducible and close the release provenance
gaps:

- Pin the Rust toolchain to an exact version (`rust-toolchain.toml`
  `channel = "1.91.0"`) so every build path uses the same compiler as the image.
- Digest-pin both base images in `docker/Dockerfile` (`rust:1.91.0-trixie` and
  `debian:trixie-slim`), bumped deliberately. Refresh a digest with
  `docker buildx imagetools inspect <image:tag> --format '{{.Manifest.Digest}}'`.
- Derive `SOURCE_DATE_EPOCH` from the commit time and add `rewrite-timestamp` to
  the buildx image exporter, so layer mtimes and the image `created` field are
  deterministic.
- Remove apt/dpkg logs and the ldconfig aux-cache in each apt step. These embed
  wall-clock timestamps in their file *contents*, which `rewrite-timestamp` (an
  mtime tool) cannot normalize; they were the only thing left keeping two runtime
  image digests from matching.
- Set `RUSTFLAGS=--remap-path-prefix` in the release stage so absolute build
  paths do not leak into the binary.
- Build releases with `no_cache: true`. This is a provenance-integrity measure
  against the shared Docker Build Cloud cache (cache-poisoning detection is
  currently suppressed), not a reproducibility measure: a correct reproducible
  build is cache-independent. The durable fix is to isolate the release cache,
  after which the `no_cache` lever and the zizmor suppression can both be removed.
- Verify reproducibility by building the release target twice from scratch and
  asserting the binary and image hashes match. This is the gate that makes the
  reproducibility claim real and catches regressions from a dependency or base bump.
- Sign and attest release images: `actions/attest-build-provenance` (signed SLSA
  provenance) plus a Cosign keyless signature on the final multi-arch digest, in the
  `merge` job and gated to production releases. main/dispatch builds stay unsigned.

`codegen-units` is intentionally left at the cargo default. The double-build check
confirms the `zebrad` binary reproduces bit-for-bit at the default with thin LTO,
so forcing `codegen-units = 1` (which costs build time) is unnecessary here.

### Expected Consequences

- Two builds of the same commit produce the same `zebrad` binary; a double-build
  hash comparison catches regressions from dependency or base-image bumps.
- The guarantee is strongest on the binary and, within a day, on the per-arch
  image filesystem. The per-arch image digest can still drift across days because
  `adduser` records the account creation date in `/etc/shadow`; pinning that date
  is a possible follow-up. The released multi-arch index digest is not reproducible
  by design, since its provenance and SBOM attestations embed build times.
- The toolchain and base images must now be bumped deliberately, which is the
  point: changes to the compiler or base are explicit and reviewable.
- Released binaries report their commit.
- Release images carry signed provenance and a Cosign signature, verifiable with
  `gh attestation verify` or `cosign verify`. This is signed (L2-grade) provenance:
  formal SLSA L3 additionally needs isolated provenance generation and an isolated
  release cache, tracked separately.

## More Information

Verify a release image (`<version>` is the release tag, e.g. `5.2.0`):

```sh
gh attestation verify oci://docker.io/zfnd/zebra:<version> --owner ZcashFoundation
cosign verify docker.io/zfnd/zebra:<version> \
  --certificate-identity-regexp='^https://github.com/ZcashFoundation/zebra/' \
  --certificate-oidc-issuer='https://token.actions.githubusercontent.com'
```

- Reproducible builds: <https://reproducible-builds.org/docs/>
- SLSA v1.0 levels and FAQ: <https://slsa.dev/spec/v1.0/levels>,
  <https://slsa.dev/spec/v1.0/faq>
- BuildKit reproducibility (`SOURCE_DATE_EPOCH`, `rewrite-timestamp`):
  <https://docs.docker.com/build/building/reproducible-builds/>

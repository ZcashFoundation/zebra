# Zebra Docker Builds

This directory contains two Dockerfiles that produce Zebra container images using
different build strategies.

## Standard Build (`Dockerfile`)

The standard build uses the official Rust Docker image (`rust:<version>-trixie`)
and Debian (`debian:trixie-slim`) as its base layers. It installs build
dependencies from the Debian package archive via `apt-get` and produces a
container image suitable for general development and testing.

## Deterministic and Full-source Bootstrapped Build (`Dockerfile.deterministic`)

While the convential approach remains standard, deterministic and bootstrapped builds
address several topics of concern for mission critical applications.

A conventional Docker build pulls opaque binary blobs from upstream registries, and
a user must trust that the images were built honestly. However, there is no practical
way to verify that the binaries inside them correspond to the published source code.
For example, an attacker who compromises an upstream image, a mirror, or the build
infrastructure could inject malicious code that is invisible to anyone who does not
reproduce the build from scratch. For operationally essential deployments, higher
levels of confidence in the integrity of the entire build may be desirable.

The deterministic and bootstrapped build replaces build dependencies with
packages from [StageX](https://stagex.tools), a collection of OCI images that
are **full-source bootstrapped** and **bit-for-bit reproducible**, which makes
them more suitable for use-cases where system integrity is non-negotiable.

StageX confronts risks that exist in standard builds via several strategies:

1. **Full-source bootstrap.** Every package in the StageX dependency tree is
   compiled from source, starting from a minimal, auditable seed binary. There
   are no opaque binary blobs in the chain. This closes the
   [trusting trust](https://www.cs.cmu.edu/~rdriley/487/papers/Thompson_1984_RessureflectionsonTrustingTrust.pdf)
   gap for the entire toolchain: compiler, linker, C library, and all dependencies.

2. **Bit-for-bit reproducibility.** Given the same inputs, the build produces
   the same outputs bit-for-bit. This means anyone can independently rebuild
   the image and verify that the result matches a published hash, without trusting
   the original builder.

3. **Release artifact signing.** Before publication, every release artifact must be
   independently reproduced by at least two maintainers on diverse hardware (at minimum
   Intel and AMD chipsets) and co-signed with their PGP keys.

4. **Source tree signing.** Every change to the StageX source tree must be reviewed
   and cryptographically signed by at least two maintainers before it can be merged.
   A single compromised or malicious maintainer cannot unilaterally modify the distribution.

5. **Minimalism.** The runtime image is built `FROM scratch` with only BusyBox and the
   `zebrad` binary. StageX uses musl libc instead of glibc and LLVM/Clang instead of
   GCC, reducing the trusted computing base and attack surface compared to conventional
   distributions.

Additionally, StageX signing keys for both release and source tree signing follow strict
key management requirements: private key material must never be exposed to an
internet-connected environment; keys must be stored on a hardware security device (e.g.
YubiKey 5 series, NitroKey 3, or a Split GPG Qubes setup); PIN protection with a non-default
PIN must be enabled; and physical touch must be required for every signing operation.

To learn more, refer to the [StageX](https://codeberg.org/stagex/stagex/src/branch/main)
repository and [paper](https://codeberg.org/stagex/whitepapers/).

### Build

Use the included `build.sh` script, which handles OCI output and binary
extraction:

```sh
./docker/build.sh
```

Or build directly:

```sh
docker build -f docker/Dockerfile.deterministic . \
  --target runtime \
  --output type=oci,rewrite-timestamp=true,force-compression=true,dest=build/oci/zebra.tar,name=zebra
```

### Verifying a build

Because the build is reproducible, if maintainer-signed hashes are published, anyone
can verify a published image.

```sh
# Rebuild from source
./docker/build.sh

# Look for the "manifest" hash value Docker outputs

# You may also hash the binary
sha256sum build/oci/zebra.tar
```

If the hashes match, you have cryptographic assurance that the image contains
exactly the code in the source repository.

---
status: accepted
date: 2026-02-04
supersedes: "[ADR-0002](0002-docker-use-gosu.md)"
story: Volumes permissions and privilege management in container entrypoint
---

# Use setpriv for Privilege Dropping in Entrypoint

## Context & Problem Statement

We previously used `gosu` for privilege dropping in our Docker entrypoint (see [ADR-0002](0002-docker-use-gosu.md)). In February 2026, `tianon/gosu:trixie` lost arm64 support, breaking our multi-architecture Docker builds.

We need a privilege-dropping solution that:

- Works on all target architectures (amd64, arm64)
- Provides equivalent functionality to gosu
- Minimizes external dependencies

## Priorities & Constraints

- Support multi-architecture builds (amd64, arm64)
- Minimize external dependencies
- Maintain equivalent privilege-dropping behavior
- Avoid complex signal handling and TTY issues

## Considered Options

- Option 1: Pin `gosu` to version tag (`tianon/gosu:1.19`) instead of Debian tag
- Option 2: Use `setpriv` from util-linux

## Decision Outcome

Chosen option: [Option 2: Use `setpriv` from util-linux]

We chose `setpriv` because:

1. **Zero additional dependencies**: `setpriv` is part of `util-linux`, already included in `debian:trixie-slim`.
2. **Native multi-arch support**: Works on all architectures supported by the base image.
3. **Equivalent functionality**: `setpriv --reuid --regid --init-groups` provides the same privilege-dropping behavior as `gosu`.
4. **Reduced attack surface**: Eliminates an external dependency, reducing supply chain risks.

### Why not pin gosu version?

Pinning to `tianon/gosu:1.19` would restore arm64 support, but:

- Still requires pulling an external image during build
- Adds supply chain dependency we can eliminate
- `setpriv` is already available at no cost

### Expected Consequences

- Reliable multi-architecture Docker builds
- No external image dependencies for privilege dropping
- Equivalent security posture to the previous gosu approach

## More Information

- [setpriv man page](https://man7.org/linux/man-pages/man1/setpriv.1.html)
- Usage: `exec setpriv --reuid="${UID}" --regid="${GID}" --init-groups "$@"`

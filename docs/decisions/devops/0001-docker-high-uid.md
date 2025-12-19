---
status: accepted
date: 2025-02-28
story: Appropriate UID/GID values for container users
---

# Use High UID/GID Values for Container Users

## Context & Problem Statement

Docker containers share the host's user namespace by default. If container UIDs/GIDs overlap with privileged host accounts, this could lead to privilege escalation if a container escape vulnerability is exploited. Low UIDs (especially in the system user range of 100-999) are particularly risky as they often map to privileged system users on the host.

Our previous approach used UID/GID 101 with the `--system` flag for user creation, which falls within the system user range and could potentially overlap with critical system users on the host.

## Priorities & Constraints

- Enhance security by reducing the risk of container user namespace overlaps
- Avoid warnings during container build related to system user ranges
- Maintain compatibility with common Docker practices
- Prevent potential privilege escalation in case of container escape

## Considered Options

- Option 1: Keep using low UID/GID (101) with `--system` flag
- Option 2: Use UID/GID (1000+) without `--system` flag
- Option 3: Use high UID/GID (10000+) without `--system` flag

## Decision Outcome

Chosen option: [Option 3: Use high UID/GID (10000+) without `--system` flag]

We decided to:

1. Change the default UID/GID from 101 to 10001
2. Remove the `--system` flag from user/group creation commands
3. Document the security rationale for these changes

This approach significantly reduces the risk of UID/GID collision with host system users while avoiding build-time warnings related to system user ranges. Using a very high UID/GID (10001) provides an additional security boundary in containers where user namespaces are shared with the host.

### Expected Consequences

- Improved security posture by reducing the risk of container escapes leading to privilege escalation
- Elimination of build-time warnings related to system user UID/GID ranges
- Consistency with industry best practices for container security
- No functional impact on container operation, as the internal user permissions remain the same

## More Information

- [NGINX Docker User ID Issue](https://github.com/nginxinc/docker-nginx/issues/490) - Demonstrates the risks of using UID 101 which overlaps with `systemd-network` user on Debian systems
- [.NET Docker Issue on System Users](https://github.com/dotnet/dotnet-docker/issues/4624) - Details the problems with using `--system` flag and the SYS_UID_MAX warnings
- [Docker Security Best Practices](https://docs.docker.com/develop/security-best-practices/) - General security recommendations for Docker containers

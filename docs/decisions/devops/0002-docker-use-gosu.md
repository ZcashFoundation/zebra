---
status: superseded by [ADR-0004](0004-docker-use-setpriv.md)
date: 2025-02-28
story: Volumes permissions and privilege management in container entrypoint
---

# Use gosu for Privilege Dropping in Entrypoint

## Context & Problem Statement

Running containerized applications as the root user is a security risk. If an attacker compromises the application, they gain root access within the container, potentially facilitating a container escape. However, some operations during container startup, such as creating directories or modifying file permissions in locations not owned by the application user, require root privileges. We need a way to perform these initial setup tasks as root, but then switch to a non-privileged user _before_ executing the main application (`zebrad`). Using `USER` in the Dockerfile is insufficient because it applies to the entire runtime, and we need to change permissions _after_ volumes are mounted.

## Priorities & Constraints

- Minimize the security risk by running the main application (`zebrad`) as a non-privileged user.
- Allow initial setup tasks (file/directory creation, permission changes) that require root privileges.
- Maintain a clean and efficient entrypoint script.
- Avoid complex signal handling and TTY issues associated with `su` and `sudo`.
- Ensure 1:1 parity with Docker's `--user` flag behavior.

## Considered Options

- Option 1: Use `USER` directive in Dockerfile.
- Option 2: Use `su` within the entrypoint script.
- Option 3: Use `sudo` within the entrypoint script.
- Option 4: Use `gosu` within the entrypoint script.
- Option 5: Use `chroot --userspec`
- Option 6: Use `setpriv`

## Decision Outcome

Chosen option: [Option 4: Use `gosu` within the entrypoint script]

We chose to use `gosu` because it provides a simple and secure way to drop privileges from root to a non-privileged user _after_ performing necessary setup tasks. `gosu` avoids the TTY and signal-handling complexities of `su` and `sudo`. It's designed specifically for this use case (dropping privileges in container entrypoints) and leverages the same underlying mechanisms as Docker itself for user/group handling, ensuring consistent behavior.

### Expected Consequences

- Improved security by running `zebrad` as a non-privileged user.
- Simplified entrypoint script compared to using `su` or `sudo`.
- Avoidance of TTY and signal-handling issues.
- Consistent behavior with Docker's `--user` flag.
- No negative impact on functionality, as initial setup tasks can still be performed.

## More Information

- [gosu GitHub repository](https://github.com/tianon/gosu#why) - Explains the rationale behind `gosu` and its advantages over `su` and `sudo`.
- [gosu usage warning](https://github.com/tianon/gosu#warning) - Highlights the core use case (stepping down from root) and potential vulnerabilities in other scenarios.
- Alternatives considered:
  - `chroot --userspec`: While functional, it's less common and less directly suited to this specific task than `gosu`.
  - `setpriv`: A viable alternative, but `gosu` is already well-established in our workflow and offers the desired functionality with a smaller footprint than a full `util-linux` installation.
  - `su-exec`: Another minimal alternative, but it has known parser bugs that could lead to unexpected root execution.

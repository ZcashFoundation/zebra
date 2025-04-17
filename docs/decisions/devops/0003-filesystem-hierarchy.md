---
status: proposed
date: 2025-02-28
story: Standardize filesystem hierarchy for Zebra deployments
---

# Standardize Filesystem Hierarchy: FHS vs. XDG

## Context & Problem Statement

Zebra currently has inconsistencies in its filesystem layout, particularly regarding where configuration, data, cache files, and binaries are stored. We need a standardized approach compatible with:

1. Traditional Linux systems.
2. Containerized deployments (Docker).
3. Cloud environments with stricter filesystem restrictions (e.g., Google's Container-Optimized OS).

We previously considered using the Filesystem Hierarchy Standard (FHS) exclusively ([Issue #3432](https://github.com/ZcashFoundation/zebra/issues/3432)). However, recent changes introduced the XDG Base Directory Specification, which offers a user-centric approach. We need to decide whether to:

* Adhere to FHS.
* Adopt XDG Base Directory Specification.
* Use a hybrid approach, leveraging the strengths of both.

The choice impacts how we structure our Docker images, where configuration files are located, and how users interact with Zebra in different environments.

## Priorities & Constraints

* **Security:** Minimize the risk of privilege escalation by adhering to least-privilege principles.
* **Maintainability:** Ensure a clear and consistent filesystem layout that is easy to understand and maintain.
* **Compatibility:** Work seamlessly across various Linux distributions, Docker, and cloud environments (particularly those with restricted filesystems like Google's Container-Optimized OS).
* **User Experience:** Provide a predictable and user-friendly experience for locating configuration and data files.
* **Flexibility:** Allow users to override default locations via environment variables where appropriate.
* **Avoid Breaking Changes:** Minimize disruption to existing users and deployments, if possible.

## Considered Options

### Option 1: FHS

* Configuration: `/etc/zebrad/`
* Data: `/var/lib/zebrad/`
* Cache: `/var/cache/zebrad/`
* Logs: `/var/log/zebrad/`
* Binary: `/opt/zebra/bin/zebrad` or `/usr/local/bin/zebrad`

### Option 2: XDG Base Directory Specification

* Configuration: `$HOME/.config/zebrad/`
* Data: `$HOME/.local/share/zebrad/`
* Cache: `$HOME/.cache/zebrad/`
* State: `$HOME/.local/state/zebrad/`
* Binary: `$HOME/.local/bin/zebrad` or `/usr/local/bin/zebrad`

### Option 3: Hybrid Approach (FHS for System-Wide, XDG for User-Specific)

* System-wide configuration: `/etc/zebrad/`
* User-specific configuration: `$XDG_CONFIG_HOME/zebrad/`
* System-wide data (read-only, shared): `/usr/share/zebrad/` (e.g., checkpoints)
* User-specific data: `$XDG_DATA_HOME/zebrad/`
* Cache: `$XDG_CACHE_HOME/zebrad/`
* State: `$XDG_STATE_HOME/zebrad/`
* Runtime: `$XDG_RUNTIME_DIR/zebrad/`
* Binary: `/opt/zebra/bin/zebrad` (system-wide) or `$HOME/.local/bin/zebrad` (user-specific)

## Pros and Cons of the Options

### FHS

* **Pros:**
  * Traditional and well-understood by system administrators.
  * Clear separation of configuration, data, cache, and binaries.
  * Suitable for packaged software installations.

* **Cons:**
  * Less user-friendly; requires root access to modify configuration.
  * Can conflict with stricter cloud environments restricting writes to `/etc` and `/var`.
  * Doesn't handle multi-user scenarios as gracefully as XDG.

### XDG Base Directory Specification

* **Pros:**
  * User-centric: configuration and data stored in user-writable locations.
  * Better suited for containerized and cloud environments.
  * Handles multi-user scenarios gracefully.
  * Clear separation of configuration, data, cache, and state.

* **Cons:**
  * Less traditional; might be unfamiliar to some system administrators.
  * Requires environment variables to be set correctly.
  * Binary placement less standardized.

### Hybrid Approach (FHS for System-Wide, XDG for User-Specific)

* **Pros:**
  * Combines strengths of FHS and XDG.
  * Allows system-wide defaults while prioritizing user-specific configurations.
  * Flexible and adaptable to different deployment scenarios.
  * Clear binary placement in `/opt`.

* **Cons:**
  * More complex than either FHS or XDG alone.
  * Requires careful consideration of precedence rules.

## Decision Outcome

Pending

## Expected Consequences

Pending

## More Information

* [Filesystem Hierarchy Standard (FHS) v3.0](https://refspecs.linuxfoundation.org/FHS_3.0/fhs-3.0.html)
* [XDG Base Directory Specification](https://specifications.freedesktop.org/basedir-spec/latest/)
* [Zebra Issue #3432: Use the Filesystem Hierarchy Standard (FHS) for deployments and artifacts](https://github.com/ZcashFoundation/zebra/issues/3432)
* [Google Container-Optimized OS: Working with the File System](https://cloud.google.com/container-optimized-os/docs/concepts/disks-and-filesystem#working_with_the_file_system)

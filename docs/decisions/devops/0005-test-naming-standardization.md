---
status: proposed
date: 2025-06-05
story: Test naming inconsistency across workflows, Docker, and Rust code makes testing difficult for developers
---

# Standardize Test Execution Interface with CLI Commands

## Context and Problem Statement

Our test system uses three different naming conventions that don't align: GitHub workflow job names (e.g., "Zebra checkpoint"), Docker environment variables (e.g., `SYNC_TO_CHECKPOINT=1`), and Rust test function names (e.g., `sync_to_checkpoint_mainnet`). This creates a complex mapping that makes it hard to find tests, debug failures, run tests locally, and onboard new contributors.

The current system requires developers to understand environment variable mappings in `docker/entrypoint.sh` to translate between workflow names and actual test execution, making local development and CI debugging unnecessarily complicated.

## Priorities & Constraints

* CI job names require manual admin updates to branch protection rules
* Test IDs must stay under 20 characters to avoid bug #7559 regression
* Some environment variables are used in Rust code and can't be changed easily
* Must maintain backward compatibility during transition
* Branch protection rule updates require separate manual admin work
* Solution should work identically in local development and CI environments

## Considered Options

* Option 1: CLI-based test runner with `zebra test` subcommand
* Option 2: Standardized environment variables with consistent naming
* Option 3: Configuration file approach using TOML/YAML
* Option 4: Keep current system and improve documentation

### Pros and Cons of the Options

#### Option 1: CLI-based test runner

* Good, because provides single interface for local and CI testing
* Good, because self-documenting through `--help` commands
* Good, because avoids environment variable complexity
* Good, because extensible without touching workflow files
* Good, because follows standard CLI design patterns
* Bad, because requires implementing new command parsing logic
* Bad, because adds another interface alongside existing environment variables during transition

#### Option 2: Standardized environment variables

* Good, because minimal code changes required
* Good, because fits current Docker-based testing approach
* Bad, because environment variables remain opaque and hard to discover
* Bad, because doesn't solve local development usability issues
* Bad, because still requires documentation to understand mappings

#### Option 3: Configuration file approach

* Good, because centralized test configuration
* Good, because can include complex test parameters
* Bad, because adds file management complexity
* Bad, because doesn't solve discoverability issues
* Bad, because requires config file distribution in Docker containers

#### Option 4: Keep current system

* Good, because no implementation work required
* Bad, because doesn't address core usability problems
* Bad, because continues to confuse developers and slow onboarding
* Bad, because makes adding new tests unnecessarily complex

## Decision Outcome

Chosen option: **Option 1: CLI-based test runner**

This approach provides the best developer experience by creating a unified, self-documenting interface for test execution. The CLI design follows established patterns and makes test discovery and execution intuitive for both local development and CI environments.

Implementation will follow a step-by-step approach:
1. Add `zebra test` subcommand alongside existing environment variables
2. Update Docker entrypoint to support both CLI and environment variable interfaces  
3. Migrate workflows to use CLI commands while keeping job names unchanged
4. Clean up naming consistency and remove old environment variables (separate PR)

### Expected Consequences

* Positive: Developers can easily discover and run tests using `zebra test --help`
* Positive: Same commands work locally and in CI without translation
* Positive: Adding new tests becomes simpler and doesn't require workflow changes
* Positive: Test execution becomes self-documenting
* Neutral: Temporary complexity during transition period with dual interfaces
* Neutral: Will require updating workflow files and documentation

## More Information

* [CLI Design Guidelines](https://clig.dev/) - Best practices for command-line interface design
* [Test Organization Patterns](https://martinfowler.com/articles/practical-test-pyramid.html) - Martin Fowler on test structure
* [GitHub Issue: Test naming standardization](../../../test-naming-standardization-issue.md) - Detailed analysis and current mappings
* [Mass Rename Documentation](https://github.com/ZcashFoundation/zebra/blob/main/book/src/dev/mass-renames.md) - Process for consistent renaming across codebase 
# Zebra CI/CD Architecture

This document provides a comprehensive overview of Zebra's Continuous Integration and Continuous Deployment (CI/CD) system. It serves as a guide for contributors, maintainers, and new team members.

## Table of Contents

1. [System Overview](#system-overview)
2. [CI/CD Workflow Diagram](#cicd-workflow-diagram)
3. [Core Infrastructure](#core-infrastructure)
4. [Workflow Organization](#workflow-organization)
5. [Test Execution Strategy](#test-execution-strategy)
6. [Infrastructure Details](#infrastructure-details)
7. [Best Practices](#best-practices)
8. [Known Issues](#known-issues)

## System Overview

Zebra's CI/CD system is built on GitHub Actions, providing a unified platform for automation. The system ensures code quality, maintains stability, and automates routine tasks through specialized workflows.

## CI/CD Workflow Diagram

Below is a simplified Mermaid diagram showing the current workflows, their key triggers, and major dependencies.

```mermaid
graph TB
  %% Triggers
  subgraph Triggers
    PR[Pull Request] & Push[Push to main] & Schedule[Weekly] & Manual[Manual]
  end

  %% Reusable build
  subgraph Build
    BuildDocker[zfnd-build-docker-image.yml]
  end

  %% CI workflows
  subgraph CI
    Unit[tests-unit.yml]
    Lint[lint.yml]
    Coverage[coverage.yml]
    DockerCfg[test-docker.yml]
    CrateBuild[test-crates.yml]
    Docs[book.yml]
    Security[zizmor.yml]
  end

  %% Integration tests on GCP
  subgraph GCP Integration
    IT[zfnd-ci-integration-tests-gcp.yml]
    FindDisks[zfnd-find-cached-disks.yml]
    Deploy[zfnd-deploy-integration-tests-gcp.yml]
    DeployNodes[zfnd-deploy-nodes-gcp.yml]
    Cleanup[zfnd-delete-gcp-resources.yml]
  end

  %% Trigger wiring
  PR --> Unit & Lint & DockerCfg & CrateBuild & IT & Security
  Push --> Unit & Lint & Coverage & Docs & Security
  Schedule --> IT
  Manual --> IT & DeployNodes & Cleanup

  %% Build dependency
  BuildDocker --> IT
  IT --> FindDisks --> Deploy

  %% Styling
  classDef primary fill:#2374ab,stroke:#2374ab,color:white
  classDef secondary fill:#48a9a6,stroke:#48a9a6,color:white
  classDef trigger fill:#95a5a6,stroke:#95a5a6,color:white
  class BuildDocker primary
  class Unit,Lint,Coverage,DockerCfg,CrateBuild,Docs,Security secondary
  class IT,FindDisks,Deploy,DeployNodes,Cleanup secondary
  class PR,Push,Schedule,Manual trigger
```

*The diagram above illustrates the parallel execution patterns in our CI/CD system. All triggers can initiate the pipeline concurrently, unit tests run in parallel after the Docker image build, and integration tests follow a mix of parallel and sequential steps. The infrastructure components support their respective workflow parts concurrently.*

## Core Infrastructure

### 1. GitHub Actions

- Primary CI/CD platform
- Workflow automation and orchestration
- Integration with other services

### 2. Infrastructure as Code

- Uses [Cloud Foundation Fabric](https://github.com/ZcashFoundation/cloud-foundation-fabric) for GCP infrastructure
- Terraform-based architecture, networking, and permissions
- Resources (VMs, Disks, Images, etc.) deployed via GitHub Actions pipelines

### 3. Build and Registry Services

#### Docker-based Testing

- Most tests run in containers defined by our [Dockerfile](http://../../docker/Dockerfile)
- The [entrypoint script](http://../../docker/entrypoint.sh) manages:
  - Test execution
  - Environment configuration
  - Resource cleanup

#### [Docker Build Cloud](https://www.docker.com/products/build-cloud/)

- Optimized build times (~10 min for non-cached, ~30 sec for cached)
- More efficient than GitHub Runners
- Addresses [Rust caching limitations](https://github.com/ZcashFoundation/zebra/issues/6169#issuecomment-1712776391)

#### Container Registries

- Google Cloud Registry: Internal CI artifacts
- [Docker Hub](https://hub.docker.com/): Public release artifacts
- Ensures proper artifact distribution

### 4. Test Infrastructure

#### GitHub-hosted Runners

- All Unit Tests jobs
- Standard CI/CD operations
- Limited to 6-hour runtime

#### Self-hosted Runners (GKE)

- All Integration Tests jobs (deployed to GCP)
- Support for tests exceeding 6 hours
- Extended logging capabilities
- Full GitHub Actions console integration

**Note**: Self-hosted Runners are just used to keep the logs running in the GitHub Actions UI for over 6 hours, the Integration Tests are not run in the Self-hosted Runner itself, but in the deployed VMs in GCP through GitHub Actions.

### 5. Queue Management

[Mergify](https://mergify.yml)

- Automated PR merging and queue-based testing
- Priority management
- Ensures code quality before merge
- See our [`.mergify.yml`](http://../../.mergify.yml) for configuration

## Workflow Organization

### Main Workflows

- **Unit Tests** (`tests-unit.yml`): OS matrix unit tests via nextest
- **Lint** (`lint.yml`): Clippy, fmt, deny, features, docs build checks
- **Coverage** (`coverage.yml`): llvm-cov with nextest, uploads to Codecov
- **Test Docker Config** (`test-docker.yml`): Validates zebrad configs against built test image
- **Test Crate Build** (`test-crates.yml`): Builds each crate under various feature sets
- **Docs (Book + internal)** (`book.yml`): Builds mdBook and internal rustdoc, publishes to Pages
- **Security Analysis** (`zizmor.yml`): GitHub Actions security lint (SARIF)
- **Release Binaries** (`release-binaries.yml`): Build and publish release artifacts
- **Release Drafter** (`release-drafter.yml`): Automates release notes
- **Integration Tests on GCP** (`zfnd-ci-integration-tests-gcp.yml`): Stateful tests, cached disks, lwd flows

### Supporting/Re-usable Workflows

- **Build docker image** (`zfnd-build-docker-image.yml`): Reusable image build with caching and tagging
- **Find cached disks** (`zfnd-find-cached-disks.yml`): Discovers GCP disks for stateful tests
- **Deploy integration tests** (`zfnd-deploy-integration-tests-gcp.yml`): Orchestrates GCP VMs and test runs
- **Deploy nodes** (`zfnd-deploy-nodes-gcp.yml`): Provision long-lived nodes
- **Delete GCP resources** (`zfnd-delete-gcp-resources.yml`): Cleanup utilities
- Helper scripts in `.github/workflows/scripts/` used by the above

## Test Execution Strategy

### Test Orchestration with Nextest

Our test execution is centralized through our Docker [entrypoint script](http://../../docker/entrypoint.sh) and orchestrated by `cargo nextest`. This provides a unified and efficient way to run tests both in CI and locally.

#### Nextest Profile-driven Testing

We use `nextest` profiles defined in [`.config/nextest.toml`](../../.config/nextest.toml) to manage test suites. A single environment variable, `NEXTEST_PROFILE`, selects the profile to run.

```bash
# Run the full test suite using the 'all-tests' profile
docker run --rm -e NEXTEST_PROFILE=all-tests zebra-tests

# Run a specific test suite, like the lightwalletd integration tests
docker run --rm -e NEXTEST_PROFILE=lwd-integration zebra-tests
```

#### Test Categories

Our tests are organized into different categories:

- **Unit & Integration Tests**: Basic functionality and component testing
- **Network Sync Tests**: Testing blockchain synchronization from various states
- **Lightwalletd Tests**: Integration with the lightwalletd service
- **RPC Tests**: JSON-RPC endpoint functionality
- **Checkpoint Tests**: Blockchain checkpoint generation and validation

Each test category has specific profiles that can be run individually using the `NEXTEST_PROFILE` environment variable.


### Pull Request Testing

#### Continuous Validation

- Tests run automatically on each commit
- Contributors get immediate feedback on their changes
- Regressions are caught early in the development process
- Reduces manual testing burden on reviewers

#### Fast Feedback Loop

- Linting: Code style and formatting
- Unit tests: Function and component behavior
- Basic integration tests: Core functionality
- All results are reported directly in the PR interface

#### Deep Validation

- Full integration test suite
- Cross-platform compatibility checks
- Performance benchmarks
- State management validation

### Scheduled Testing

Weekly runs include:

- Full Mainnet synchronization
- Extended integration suites
- Resource cleanup

## Infrastructure Details

### VM-based Test Infrastructure

#### Test-specific Requirements

- Some integration tests need a fully synced network
- Certain tests validate against specific chain heights
- Network state persistence between test runs
- Not all tests require this infrastructure - many run in standard containers

#### State Management Complexity

- **Creation**: Initial sync and state building for test environments
- **Versioning**: Multiple state versions for different test scenarios
- **Caching**: Reuse of existing states to avoid re-sync
- **Attachment**: Dynamic VM disk mounting for tests
- **Cleanup**: Automated state and resource cleanup

#### Infrastructure Implications

- GCP VM infrastructure for state-dependent tests
- Complex disk image management for test states
- State versioning and compatibility checks
- Resource lifecycle management

#### Future Considerations

- Potential migration of state-dependent tests to container-native environments
- Would require solving state persistence in Kubernetes
- Need to balance containerization benefits with test requirements
- Opportunity to reduce infrastructure complexity

## Best Practices

### For Contributors

#### Local Testing

```bash
# Build and run tests
docker build -t zebra-tests --target tests .
docker run --rm zebra-tests
```

#### PR Guidelines

- Use descriptive labels
- Mark as draft when needed
- Address test failures

### For Maintainers

#### Workflow Maintenance

- Regular review of workflow efficiency
- Update resource allocations as needed
- Monitor test execution times

#### Security Considerations

- Regular secret rotation
- Access control review
- Dependency updates

## Known Issues

### External Contributor Limitations

#### GCP Dependencies

- Most CI workflows depend on Google Cloud Platform resources
- Docker artifacts and VM images are tied to GCP
- External contributors cannot run full CI suite in their forks
- Integration tests require GCP infrastructure access
- This particularly impacts:
  - Integration test execution
  - Node deployment testing
  - State storage and caching validation

#### GitHub Actions Variables Restriction

- Due to a [GitHub Actions limitation](https://github.com/orgs/community/discussions/44322), workflows in forked repositories cannot access repository variables
- This affects external contributors' ability to run CI workflows
- Required configuration values are not available in fork-based PRs
- Currently no workaround available from GitHub
- Impact on external contributors:
  - Cannot run workflows requiring GCP credentials
  - Unable to access configuration variables
  - Limited ability to test infrastructure changes

### Mitigation Through Mergify

- When external PRs enter the merge queue, they are tested with full access to variables and resources
- All CI workflows run in the context of our repository, not the fork
- This provides a safety net, ensuring no untested code reaches production
- External contributors can still get feedback through code review before their changes are tested in the queue

These safeguards help maintain code quality while working around the platform limitations for external contributions.

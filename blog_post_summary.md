# Zebra CI/CD Infrastructure: A Major Modernization Journey

*This blog post highlights the significant improvements made to Zebra's Continuous Integration and Continuous Deployment infrastructure over the past few months, showcasing how thoughtful engineering can transform development workflows.*

## Introduction

The Zcash Foundation's Zebra project has undergone a remarkable transformation in its CI/CD infrastructure, thanks to the dedicated work of Gustavo Valverde. Over the past few months, Gustavo has systematically modernized and optimized Zebra's development pipeline, resulting in faster builds, more reliable tests, and a significantly improved developer experience.

## The Challenge

Before these improvements, Zebra's CI/CD pipeline faced several critical challenges:

- **Fragmented Test Execution**: Individual environment variables controlled different test suites, leading to inconsistent configurations and complex workflow management
- **Feature Gate Inefficiencies**: Different feature combinations between build-time and runtime caused unnecessary Rust rebuilds
- **Complex Entrypoint Logic**: The entrypoint script contained complex conditional logic that was hard to maintain
- **Resource Waste**: Long-running sync operations were frequently cancelled by PR activity, wasting hours of computation
- **Inconsistent Naming**: Workflow jobs, environment variables, and Rust functions used different naming conventions, making debugging difficult

## Key Improvements

### 1. **Nextest Integration: A Game-Changer for Test Performance**

The most significant improvement comes from integrating [cargo-nextest](https://nexte.st/), a modern Rust test runner that dramatically improves test execution performance.

**What Changed:**
- Added `.config/nextest.toml` with 17 specialized test profiles
- Replaced environment variable-based test control with a single `NEXTEST_PROFILE` variable
- Centralized test filtering and scoping logic

**Impact:**
- Significant reduction in test execution times across all test suites
- Eliminated unnecessary Rust rebuilds caused by feature flag mismatches
- Streamlined CI workflow execution with unified environment variable approach

### 2. **Intelligent Concurrency Management**

One of the most frustrating issues was that long-running sync operations (which could take hours) would be cancelled whenever a PR was updated or merged.

**What Changed:**
- Separate concurrency groups for scheduled, manual, and PR workflows
- Only cancel previous runs for pull request events, protecting scheduled/manual runs
- Set `cancel-in-progress: false` for long-running sync jobs

**Impact:**
- Multi-hour sync operations are no longer interrupted by PR activity
- Maintained efficient PR workflow cancellation where appropriate
- Better resource utilization and reduced wasted computation

### 3. **Cloud Integration Simplification**

The Google Cloud Platform integration was previously complex and error-prone, involving intermediate containers and manual disk mounting.

**What Changed:**
- Replaced `busybox` + `docker run` approach with `gcloud compute instances create-with-container`
- Direct container environment variable and disk mount configuration
- Improved log checking logic and error handling

**Impact:**
- Container logs are reliably sent to GCP Cloud Logging
- Simplified workflow structure and reduced complexity
- Better debugging capabilities for cloud-based tests

### 4. **Standardized Naming Conventions**

A seemingly small but impactful change was standardizing naming conventions across the entire test infrastructure.

**What Changed:**
- Consistent patterns: `state-*`, `sync-*`, `generate-*`, `rpc-*`, `lwd-*`
- Aligned workflow job names, environment variables, and Rust function names
- Clear mapping between different layers of the testing stack

**Impact:**
- Easier debugging and test tracing
- Improved onboarding for new contributors
- Better maintainability and reduced confusion

### 5. **Enhanced State Management**

The cached state system was improved to handle edge cases and prevent corruption.

**What Changed:**
- Conditional disk searching based on test requirements
- Better job dependency management
- Improved image naming reliability using `RUNNING_DB_VERSION`

**Impact:**
- Fixed "No branch disk found" errors in tests
- Prevented creation of corrupted cached states from failed tests
- More reliable test execution and state management

## Technical Deep Dive: Nextest Integration

The nextest integration deserves special attention as it represents a fundamental shift in how Zebra approaches testing.

**Before (Traditional cargo test):**
```bash
# Multiple environment variables controlling different test aspects
RUN_ALL_TESTS=1
STATE_FAKE_ACTIVATION_HEIGHTS=1
SYNC_LARGE_CHECKPOINTS_EMPTY=1
# ... many more variables
```

**After (Nextest profiles):**
```bash
# Single variable with clear, semantic meaning
NEXTEST_PROFILE=sync-full-mainnet
```

**Benefits:**
- **Performance**: Parallel execution and smart filtering reduce test times
- **Maintainability**: Centralized configuration in `.config/nextest.toml`
- **Reliability**: Proper timeout configurations per test type
- **Debugging**: Better progress reporting and immediate success output

## Impact on Development Workflow

These improvements have transformed how developers interact with Zebra's CI/CD system:

1. **Faster Feedback**: Tests complete more quickly, providing faster feedback on changes
2. **Reliable CI**: Fewer false failures and more predictable test behavior
3. **Better Debugging**: Improved logging and error reporting make issues easier to diagnose
4. **Resource Efficiency**: Better utilization of CI resources and reduced waste
5. **Developer Onboarding**: Clearer naming and simpler workflows help new contributors

## Looking Forward

The foundation laid by these improvements opens up exciting possibilities for future enhancements:

- **Test Sharding**: Nextest's architecture enables more granular test distribution
- **Advanced Profiling**: Further optimization based on performance data
- **Enhanced Monitoring**: Better insights into CI performance and bottlenecks
- **Automated Optimization**: AI-driven suggestions for workflow improvements

## Conclusion

Gustavo Valverde's contributions to Zebra's CI/CD infrastructure represent a masterclass in systematic infrastructure improvement. By addressing both immediate pain points and laying the groundwork for future enhancements, these changes have significantly improved Zebra's development velocity and reliability.

The transformation from a fragmented, hard-to-maintain system to a modern, efficient, and reliable CI/CD pipeline demonstrates the value of investing in developer experience and infrastructure quality. As Zebra continues to evolve, this solid foundation will enable faster iteration, better testing, and more confident deployments.

These improvements serve as an excellent example for other open-source projects looking to modernize their development workflows. The combination of thoughtful design, systematic implementation, and focus on developer experience creates a virtuous cycle of improvement that benefits the entire project ecosystem.

---

*For more details on specific changes, see the [complete PR summary](gustavovalverde_prs_summary.md).*
---
# status and date are the only required elements. Feel free to remove the rest.
status: accepted
date: 2025-04-07
builds-on: N/A
story: Need a scalable and maintainable way to test various Docker image configurations derived from `.env` variables and `entrypoint.sh` logic, ensuring consistency between CI and CD pipelines. https://github.com/ZcashFoundation/zebra/pull/8948
---

# Centralize Docker Configuration Testing using a Reusable Workflow

## Context and Problem Statement

Currently, tests verifying Zebra's Docker image configuration (based on environment variables processed by `docker/entrypoint.sh`) are implemented using a reusable workflow (`sub-test-zebra-config.yml`). However, the _invocation_ of these tests, including the specific scenarios (environment variables, grep patterns), is duplicated and scattered across different workflows, notably the CI workflow (`sub-ci-unit-tests-docker.yml`) and the CD workflow (`zfnd-deploy-nodes-gcp.yml`).

This leads to:

1. **Code Duplication:** Similar test setup logic exists in multiple places.
2. **Maintenance Overhead:** Adding or modifying configuration tests requires changes in multiple files.
3. **Scalability Issues:** Adding numerous new test scenarios would significantly clutter the main CI and CD workflow files.
4. **Potential Inconsistency:** Risk of configuration tests diverging between CI and CD environments.

We need a centralized, scalable, and maintainable approach to define and run these configuration tests against Docker images built in both CI and CD contexts.

## Priorities & Constraints <!-- optional -->

- **DRY Principle:** Avoid repeating test logic and scenario definitions.
- **Maintainability:** Configuration tests should be easy to find, understand, and modify.
- **Scalability:** The solution should easily accommodate adding many more test scenarios in the future.
- **Consistency:** Ensure the same tests run against both CI and CD images where applicable.
- **Integration:** Leverage existing GitHub Actions tooling and workflows effectively.
- **Reliability:** Testing relies on running a container and grepping its logs for specific patterns to determine success.

## Considered Options

1. **Status Quo:** Continue defining and invoking configuration tests within the respective CI (`sub-ci-unit-tests-docker.yml`) and CD (`zfnd-deploy-nodes-gcp.yml`) workflows, using `sub-test-zebra-config.yml` for the core run/grep logic.
2. **Modify and Extend `sub-test-zebra-config.yml`:** Convert the existing `sub-test-zebra-config.yml` workflow. Remove its specific test inputs (`test_id`, `grep_patterns`, `test_variables`). Add multiple jobs _inside_ this workflow, each hardcoding a specific test scenario (run container + grep logs). The workflow would only take `docker_image` as input.
3. **Use `docker-compose.test.yml`:** Define test scenarios as services within a dedicated `docker-compose.test.yml` file. The CI/CD workflows would call a script (like `sub-test-zebra-config.yml`) that uses `docker compose` to run specific services and performs log grepping.
4. **Create a _New_ Dedicated Reusable Workflow:** Create a _new_ reusable workflow (e.g., `sub-test-all-configs.yml`) that takes a Docker image digest as input and contains multiple jobs, each defining and executing a specific configuration test scenario (run container + grep logs).

## Pros and Cons of the Options <!-- optional -->

### Option 1: Status Quo

- Bad: High duplication, poor maintainability, poor scalability.

### Option 2: Modify and Extend `sub-test-zebra-config.yml`

- Good: Centralizes test definition, execution, and assertion logic within the GHA ecosystem. Maximizes DRY principle for GHA workflows. High maintainability and scalability for adding tests. Clear separation of concerns (build vs. test config). Reuses an existing workflow file structure.
- Bad: Modifies the existing workflow's purpose significantly. Callers need to adapt.

### Option 3: Use `docker-compose.test.yml`

- Good: Centralizes test _environment definitions_ in a standard format (`docker-compose.yml`). Easy local testing via `docker compose`.
- Bad: Requires managing an extra file (`docker-compose.test.yml`). Still requires a GitHub Actions script/workflow step to orchestrate `docker compose` commands and perform the essential log grepping/assertion logic. Less integrated into the pure GHA workflow structure.

### Option 4: Create a _New_ Dedicated Reusable Workflow

- Good: Cleanest separation - new workflow has a clear single purpose from the start. High maintainability and scalability.
- Bad: Introduces an additional workflow file. Adds a layer of workflow call chaining.

## Decision Outcome

Chosen option [Option 2: Modify and Extend `sub-test-zebra-config.yml`]

This option provides a good balance of maintainability, scalability, and consistency by centralizing the configuration testing logic within a single, dedicated GitHub Actions reusable workflow (`sub-test-zebra-config.yml`). It directly addresses the code duplication across CI and CD pipelines and leverages GHA's native features for modularity by converting the existing workflow into a multi-job test suite runner.

While Option 4 (creating a new workflow) offered slightly cleaner separation initially, modifying the existing workflow (Option 2) achieves the same goal of centralization while minimizing the number of workflow files. It encapsulates the entire test process (definition, execution, assertion) within GHA jobs in the reused file.

The `sub-test-zebra-config.yml` workflow will be modified to remove its specific test inputs and instead contain individual jobs for each configuration scenario to be tested, taking only the `docker_image` digest as input. The CI and CD workflows will be simplified to call this modified workflow once after their respective build steps.

### Expected Consequences <!-- optional -->

- Reduction in code duplication within CI/CD workflow files.
- Improved maintainability: configuration tests are located in a single file (`sub-test-zebra-config.yml`).
- Easier addition of new configuration test scenarios by adding jobs to `sub-test-zebra-config.yml`.
- Clearer separation between image building and configuration testing logic.
- `sub-test-zebra-config.yml` will fundamentally change its structure and inputs.
- CI/CD workflows (`zfnd-deploy-nodes-gcp.yml`, parent of `sub-ci-unit-tests-docker.yml`) will need modification to remove old test jobs and add calls to the modified reusable workflow, passing the correct image digest.
- Debugging might involve tracing execution across workflow calls and within the multiple jobs of `sub-test-zebra-config.yml`.

## More Information <!-- optional -->

- GitHub Actions: Reusing Workflows: [https://docs.github.com/en/actions/using-workflows/reusing-workflows](https://docs.github.com/en/actions/using-workflows/reusing-workflows)
- Relevant files:
  - `.github/workflows/test-docker.yml` (Centralized Docker config tests with matrix strategy)
  - `.github/workflows/zfnd-deploy-nodes-gcp.yml` (CD workflow for GCP deployments)
  - `docker/entrypoint.sh` (Script processing configurations)
  - `docker/.env` (Example environment variables)

### Implementation Note (December 2025)

The decision was implemented using a different approach than originally described:

- Instead of modifying `sub-test-zebra-config.yml`, a new `test-docker.yml` workflow was created
- The workflow uses GitHub Actions matrix strategy to define multiple test scenarios
- Each scenario specifies environment variables and grep patterns for validation
- This achieves the ADR's goals of centralization, DRY, and scalability

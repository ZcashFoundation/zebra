#!/usr/bin/env bash

set -euo pipefail

: "${PROVENANCE_BUILDER_WORKFLOW:?set PROVENANCE_BUILDER_WORKFLOW}"
: "${PROVENANCE_OUTPUT:?set PROVENANCE_OUTPUT}"
: "${PROVENANCE_SOURCE_SHA:?set PROVENANCE_SOURCE_SHA}"

workflow_path="${GITHUB_WORKFLOW_REF#"${GITHUB_REPOSITORY}/"}"
workflow_path="${workflow_path%@"${GITHUB_REF}"}"

jq -n \
  --arg event_name "${GITHUB_EVENT_NAME}" \
  --arg invocation_id "${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}/attempts/${GITHUB_RUN_ATTEMPT}" \
  --arg job_workflow_ref "${GITHUB_REPOSITORY}/${PROVENANCE_BUILDER_WORKFLOW}@${GITHUB_WORKFLOW_SHA}" \
  --arg ref "${GITHUB_REF}" \
  --arg repository "${GITHUB_REPOSITORY}" \
  --arg repository_id "${GITHUB_REPOSITORY_ID}" \
  --arg repository_owner_id "${GITHUB_REPOSITORY_OWNER_ID}" \
  --arg runner_environment "${RUNNER_ENVIRONMENT}" \
  --arg server_url "${GITHUB_SERVER_URL}" \
  --arg source_sha "${PROVENANCE_SOURCE_SHA}" \
  --arg workflow_path "${workflow_path}" \
  --arg workflow_sha "${GITHUB_WORKFLOW_SHA}" \
  '{
    buildDefinition: {
      buildType: "https://actions.github.io/buildtypes/workflow/v1",
      externalParameters: {
        workflow: {
          ref: $ref,
          repository: ($server_url + "/" + $repository),
          path: $workflow_path
        }
      },
      internalParameters: {
        github: {
          event_name: $event_name,
          repository_id: $repository_id,
          repository_owner_id: $repository_owner_id,
          runner_environment: $runner_environment
        }
      },
      resolvedDependencies: [
        {
          uri: ("git+" + $server_url + "/" + $repository + "@" + $ref),
          digest: { gitCommit: $workflow_sha }
        },
        {
          uri: ("git+" + $server_url + "/" + $repository + "@" + $source_sha),
          digest: { gitCommit: $source_sha }
        }
      ]
    },
    runDetails: {
      builder: { id: ($server_url + "/" + $job_workflow_ref) },
      metadata: { invocationId: $invocation_id }
    }
  }' > "${PROVENANCE_OUTPUT}"

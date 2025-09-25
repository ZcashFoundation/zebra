## Motivation

When Zebra runs in different orchestration systems (ECS, K8s), and when oad balancers route traffic to it there's a need for simple HTTP probes to avoid routing traffic to un-healthy nodes, and Zebra has no readiness/liveness HTTP checks to validate this.

Closes https://github.com/ZcashFoundation/zebra/issues/8830.

## Solution

Use the current JSON RPC (RPSEE) server and add a simple GET health endpoint to it that returns the following details:
`curl -s http://127.0.0.1:8232/health`
response:
`{
  "status": "healthy",
  "version": "1.0.0-beta.41",
  "git_tag": "v1.0.0-rc.1",
  "git_commit": "75397cd59d0bac4b4c1e71187bb8af7494efb8b4",
  "timestamp": "2025-09-02T22:35:14.756599+00:00"
}
`

### Tests

A manual test of running the node locally and checking that the endpoint returns the health information correctly for a GET request for /health
In addition to that, those 2 snapshots tests were added:
- Mainnet test (get_health_info_status@mainnet_10.snap)
Checks that calling get_health_info_status on mainnet returns:
`{ "status": "healthy" }`

Testnet test (get_health_info_status@testnet_10.snap)
Checks that calling get_health_info_status on testnet also returns:
`{ "status": "healthy" }`


### Follow-up Work

Possible to add or adapt any information returned for that request.

### PR Checklist

<!-- Check as many boxes as possible. -->

- [ ] The PR name is suitable for the release notes.
- [ ] The PR follows the [contribution guidelines](https://github.com/ZcashFoundation/zebra/blob/main/CONTRIBUTING.md).
- [ ] The library crate changelogs are up to date.
- [x] The solution is tested.
- [x] The documentation is up to date.

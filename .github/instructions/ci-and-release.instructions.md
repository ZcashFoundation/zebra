---
applyTo: ".github/actions/**,.github/scripts/**,.github/workflows/**,deploy/**,docker/**,make/**,scripts/**,**/Dockerfile,Makefile"
excludeAgent: "cloud-agent"
---

# CI and release review

- Flag workflow inputs, required environment variables, PR-derived values, or resource identifiers that can reach a publishing, deployment, or deletion step before validation. Check that required commit identifiers are full SHAs when the operation needs an immutable revision.

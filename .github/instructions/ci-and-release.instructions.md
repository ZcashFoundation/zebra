---
applyTo: ".github/**,docker/**"
---

# Safety review rules: CI and deployment

Breaking these rules can split consensus, let peers or RPC callers exhaust the node, crash it, or corrupt its state. Flag a rule only where the diff introduces or extends the problem, give the file and line, and say what to change.

## Validate CI inputs before any side effects

Validate env vars, workflow inputs, values parsed from PR bodies, and derived identifiers at the boundary, and reject invalid or unsupported combinations before any side-effecting job runs.

Look for: `workflow_dispatch` inputs used without checks; branch names or short SHAs where an immutable full commit SHA is required; scripts reading required env vars without checking they are set; invalid inputs that only fail in a later job

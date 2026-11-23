---
applyTo: "**/*.rs"
excludeAgent: "cloud-agent"
---

# Rust safety review

- Trace new panics to their inputs. Flag panics reachable from ordinary failures, such as a state read whose record can disappear during pruning. Leave assertions for internal invariants alone when the code establishes the invariant.
- Flag operations inside `debug_assert!` whose side effects are needed in release builds.
- Check that shared entry points bound expensive work before admission to blocking tasks, proof verification, or state lookups. A limit in one caller leaves other callers uncovered.

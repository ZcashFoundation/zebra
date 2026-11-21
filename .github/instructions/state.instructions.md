---
applyTo: "zebra-state/**"
excludeAgent: "cloud-agent"
---

# State and storage review

- Check format changes against `book/src/dev/state-db-upgrades.md`. Verify that the database format version and upgrade cover changed on-disk data, that an upgrade and a fresh sync produce equivalent state, and that supported older binaries can read the result safely or refuse it cleanly. Check that new column families have documented key and value formats.
- Check that related reads use one consistent state snapshot and that mutations to related records commit atomically. A block and its spent outputs must come from the same view of the chain.
- Trace invalidation and pruning across retained forks and derived indexes. Verify that readers see a consistent set of branches when a new snapshot is published.

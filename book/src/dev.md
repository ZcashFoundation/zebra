# Developer documentation

These pages are for contributors; the [API reference](api.md) covers individual
Rust APIs.

- **Design:** [architecture](dev/overview.md) (crates, services, block and
  transaction flow), [mempool specification](dev/mempool-specification.md),
  [state database format](dev/state-db-upgrades.md). The
  [RFCs](dev/rfcs.md) are the original design records, annotated with where
  the code has diverged.
- **Contributing:** the [contribution guide](CONTRIBUTING.md);
  [property testing](dev/proptests.md),
  [profiling](dev/profiling-and-benchmarking.md),
  [tokio-console](dev/tokio-console.md),
  [private testnet tests](dev/private-testnet.md).
- **Maintenance:** [consensus refactors](dev/consensus-refactors.md),
  [mass renames](dev/mass-renames.md),
  [ECC dependency updates](dev/ecc-updates.md),
  [checkpoints](dev/zebra-checkpoints.md).
- **Process:** [CI](dev/continuous-integration.md),
  [delivery](dev/continuous-delivery.md),
  [releases](dev/release-process.md), [audits](dev/audit.md),
  [changelogs](dev/changelog-guidelines.md), [labels](dev/labels.md),
  [crates](dev/crate-owners.md).

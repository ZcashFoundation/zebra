# Summary

[Zebra](README.md)

- [User Documentation](user.md)
  - [System Requirements](user/requirements.md)
  - [Supported Platforms](user/supported-platforms.md)
    - [Platform Tier Policy](user/target-tier-policies.md)
  - [Installing Zebra](user/install.md)
  - [Running Zebra](user/run.md)
  - [Zebra with Docker](user/docker.md)
  - [Tracing Zebra](user/tracing.md)
  - [Zebra Metrics](user/metrics.md)
  - [Lightwalletd](user/lightwalletd.md)
  - [zk-SNARK Parameters](user/parameters.md)
  - [Mining](user/mining.md)
    - [Testnet Mining with s-nomp](user/mining-testnet-s-nomp.md)
    - [Mining with Zebra in Docker](user/mining-docker.md)
  - [Kibana blockchain explorer](user/elasticsearch.md)
  - [Forking the Zcash Testnet with Zebra](user/fork-zebra-testnet.md)
  - [Custom Testnets](user/custom-testnets.md)
    - [Regtest with Zebra](user/regtest.md)
  - [OpenAPI specification](user/openapi.md)
  - [Troubleshooting](user/troubleshooting.md)
- [Developer Documentation](dev.md)
  - [Contribution Guide](CONTRIBUTING.md)
  - [Design Overview](dev/overview.md)
  - [Mempool Specification](dev/mempool-specification.md)
  - [Diagrams](dev/diagrams.md)
    - [Network Architecture](dev/diagrams/zebra-network.md)
    - [Mempool Architecture](dev/diagrams/mempool-architecture.md)
  - [Upgrading the State Database](dev/state-db-upgrades.md)
  - [Zebra versioning and releases](dev/release-process.md)
  - [Continuous Integration](dev/continuous-integration.md)
  - [Continuous Delivery](dev/continuous-delivery.md)
  - [Generating Zebra Checkpoints](dev/zebra-checkpoints.md)
  - [Doing Mass Renames](dev/mass-renames.md)
  - [Updating the ECC dependencies](dev/ecc-updates.md)
  - [Running a Private Testnet Test](dev/private-testnet.md)
  - [Zebra crates](dev/crate-owners.md)
  - [Zebra RFCs](dev/rfcs.md)
    - [Pipelinable Block Lookup](dev/rfcs/0001-pipelinable-block-lookup.md)
    - [Parallel Verification](dev/rfcs/0002-parallel-verification.md)
    - [Inventory Tracking](dev/rfcs/0003-inventory-tracking.md)
    - [Asynchronous Script Verification](dev/rfcs/0004-asynchronous-script-verification.md)
    - [State Updates](dev/rfcs/0005-state-updates.md)
    - [Contextual Difficulty Validation](dev/rfcs/0006-contextual-difficulty.md)
    - [Tree States](dev/rfcs/0007-treestate.md)
    - [Zebra Client](dev/rfcs/0009-zebra-client.md)
    - [V5 Transaction](dev/rfcs/0010-v5-transaction.md)
    - [Async Rust in Zebra](dev/rfcs/0011-async-rust-in-zebra.md)
    - [Value Pools](dev/rfcs/0012-value-pools.md)
- [API Reference](api.md)

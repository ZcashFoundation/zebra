# System Requirements

We usually build `zebrad` on systems with:
- 2+ CPU cores
- 7+ GB RAM
- 14+ GB of disk space

On many-core machines (like, 32-core) the build is very fast; on 2-core machines
it's less fast.

We continuously test that our builds and tests pass on:
- Windows Server 2019
- macOS Big Sur 11.0
- Ubuntu 18.04 / the latest LTS
- Debian Buster

We usually run `zebrad` on systems with:
- 4+ CPU cores
- 16+ GB RAM
- 50GB+ available disk space for finalized state
- 100+ Mbps network connections

`zebrad` might build and run fine on smaller and slower systems - we haven't
tested its exact limits yet.

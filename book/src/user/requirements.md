# System Requirements

We usually build `zebrad` on systems with:

- 2+ CPU cores
- 7+ GB RAM
- 14+ GB of disk space

On many-core machines (like, 32-core) the build is very fast; on 2-core machines
it's less fast.

We continuously test that our builds and tests pass on:

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

# Additional Features

## Sentry Production Monitoring

Compile Zebra with `--features sentry` to monitor it using Sentry in production.

## Lightwalletd Test Requirements

To test Zebra's `lightwalletd` RPC methods:

- compile Zebra with the `--features lightwalletd-grpc-tests`
- install a `lightwalletd` binary
  - Zebra's tests currently target [adityapk00/lightwalletd](https://github.com/adityapk00/lightwalletd)
  - some tests might fail on other lightwalletd versions, due to differences in the logs
- install the `protoc` Protobuf compiler:
  - the `protobuf-compiler` or `protobuf` package, or
  - `cmake` to automatically compile `protoc` in the `zebrad` build script
- set the required test environmental variables:
  - TODO: list or link to test environmental variables - [see ticket #4363](https://github.com/ZcashFoundation/zebra/issues/4363)

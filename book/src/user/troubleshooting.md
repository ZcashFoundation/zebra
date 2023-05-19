# Troubleshooting

We continuously test that our builds and tests pass on the _latest_ [GitHub
Runners](https://docs.github.com/en/actions/using-github-hosted-runners/about-github-hosted-runners#supported-runners-and-hardware-resources)
for:

- macOS,
- Ubuntu,
- Docker:
  - Debian Bullseye.

## Memory Issues

- If Zebra's build runs out of RAM, try setting `export CARGO_BUILD_JOBS=2`.
- If Zebra's tests timeout or run out of RAM, try running `cargo test -- --test-threads=2`. Note that `cargo` uses all processor cores on your machine
  by default.

## Network Issues

- Some of Zebra's tests download Zcash blocks, so they might be unreliable
  depending on your network connection. You can set `ZEBRA_SKIP_NETWORK_TESTS=1`
  to skip the network tests.
- Zebra may be unreliable on Testnet, and under less-than-perfect network
  conditions. See our [future
  work](https://github.com/ZcashFoundation/zebra#future-work) for details.

## Issues with Tests on macOS

Some of Zebra's tests deliberately cause errors that make Zebra panic. macOS
records these panics as crash reports. If you are seeing "Crash Reporter"
dialogs during Zebra tests, you can disable them using this Terminal.app
command:

```sh
defaults write com.apple.CrashReporter DialogType none
```

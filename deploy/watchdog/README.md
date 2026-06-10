# Zebra Watchdog (`zebra-watchdog`)

A standalone Rust watchdog sidecar that queries the local Zebra services and
reports their status. It is deployed alongside `zebrad` as its own systemd
service and reports check failures and recoveries to [Sentry](https://sentry.io).

It has two run modes:

- `zebra-watchdog check` — one-shot deploy verification with a retry loop, a
  drop-in replacement for `deploy/zcashd-compat/sync-check.sh`. Exits `0` when
  all checks pass within the timeout, non-zero otherwise.
- `zebra-watchdog run` — continuous operation under systemd. Runs all checks
  on an interval forever and reports failure/recovery transitions to Sentry.

## Architecture

```text
deploy/watchdog/
  Cargo.toml                       workspace member, binary: zebra-watchdog
  sync-check.sh                    thin deploy-time wrapper around `zebra-watchdog check`
  systemd/zebra-watchdog.service   systemd unit for continuous operation
  src/
    main.rs                        CLI (check/run), tracing + Sentry init
    config.rs                      env/CLI configuration
    runner.rs                      one-shot retry loop and continuous loop
    reporting.rs                   tracing logs + Sentry transition events
    checks/
      mod.rs                       Check trait + registry (pluggable checks)
      zcashd_compat.rs             zcashd-compat sync check
```

The watchdog is intentionally **not** part of the `zebrad` runtime: it is
deploy/operator tooling that observes the node from the outside, so a `zebrad`
hang, crash, or supervisor failure cannot take the watchdog down with it.

### Checks

Checks are pluggable. Each check implements the `Check` trait
([`src/checks/mod.rs`](src/checks/mod.rs)) and returns a `CheckOutcome`
(pass/fail, a one-line summary, and structured details).

The initial registry contains a single check:

#### `zcashd_compat_sync`

Mirrors the predicates of the legacy `deploy/zcashd-compat/sync-check.sh`:

1. a `zebrad .*--zcashd-compat` process is running (`pgrep -f`),
2. a `zcashd .*-zebra-compat` process is running (`pgrep -f`),
3. zcashd `getzebracompatinfo` reports `service_state == "ready"`,
   `zebra.reachable == true`, and `zebra.identity_verified == true`,
4. `abs(zebra getblockcount - zcashd getblockcount) <= HEIGHT_MAX_DRIFT`.

RPC authentication uses the cookie files written by each node
(`user:password` content, sent as HTTP basic auth).

### Adding a new check

1. Add a module under `src/checks/` implementing the `Check` trait. Bound all
   external waits (the shared config provides an RPC timeout).
2. Register it in `registry()` in [`src/checks/mod.rs`](src/checks/mod.rs).

The runner and Sentry reporting work on `CheckOutcome` values, so no other
wiring is needed. Each check is tracked independently for failure/recovery
transitions.

## Configuration

All settings can be provided as CLI flags or environment variables. The
environment variable names match the legacy sync-check script and the
`deploy-zcashd-compat.yml` workflow.

| Environment variable      | Flag                        | Default                               | Purpose |
| ------------------------- | --------------------------- | ------------------------------------- | ------- |
| `ZEBRA_RPC_URL`           | `--zebra-rpc-url`           | `http://127.0.0.1:8232`               | Zebra JSON-RPC endpoint |
| `ZEBRA_COOKIE_FILE`       | `--zebra-cookie-file`       | `/root/.cache/zebra/.cookie`          | Zebra RPC cookie file |
| `ZCASHD_RPC_URL`          | `--zcashd-rpc-url`          | `http://[::1]:8232`                   | zcashd JSON-RPC endpoint |
| `ZCASHD_COOKIE_FILE`      | `--zcashd-cookie-file`      | `/mnt/snapshots/runtime/zcashd/.cookie` | zcashd RPC cookie file |
| `ZEBRAD_PROCESS_PATTERN`  | `--zebrad-process-pattern`  | `zebrad .*--zcashd-compat`            | `pgrep -f` pattern for zebrad |
| `ZCASHD_PROCESS_PATTERN`  | `--zcashd-process-pattern`  | `zcashd .*-zebra-compat`              | `pgrep -f` pattern for zcashd |
| `HEIGHT_MAX_DRIFT`        | `--height-max-drift`        | `10`                                  | Max allowed height drift |
| `SYNC_CHECK_TIMEOUT`      | `--sync-check-timeout`      | `600`                                 | One-shot `check` total timeout (seconds) |
| `SYNC_CHECK_INTERVAL`     | `--sync-check-interval`     | `15`                                  | One-shot `check` retry interval (seconds) |
| `WATCHDOG_INTERVAL`       | `--watchdog-interval`       | `60`                                  | Continuous `run` cycle interval (seconds) |
| `WATCHDOG_RPC_TIMEOUT`    | `--rpc-timeout`             | `30`                                  | Per-RPC-request timeout (seconds) |

Logging verbosity is controlled with the standard `RUST_LOG` environment
variable (defaults to `info`).

## Sentry reporting

Sentry is enabled when `SENTRY_DSN` is set in the environment. Without a DSN,
the watchdog logs locally (stdout/journald) and is otherwise fully functional.

| Variable             | Purpose |
| -------------------- | ------- |
| `SENTRY_DSN`         | Enables Sentry reporting (the DSN is not a secret, but treat the env file as operator config) |
| `SENTRY_ENVIRONMENT` | Optional environment name (for example `zcashd-compat-mainnet`) |
| `SENTRY_RELEASE`     | Optional release override; defaults to `zebra-watchdog@<crate version>` |

Reporting behavior is designed to avoid event spam:

- **Discrete Sentry events** are captured only on status _transitions_: when a
  check goes from passing to failing (error event) and when it recovers
  (info event). A check that fails persistently produces one event, not one
  per cycle.
- **Sentry logs** carry the per-cycle status: warnings/errors are forwarded as
  structured Sentry logs, info-level events become breadcrumbs.
- Events are tagged with `watchdog.check` and `watchdog.transition`, and carry
  the check's structured details (heights, drift, failing predicate) as extras.

## Systemd deployment

The unit file is [`systemd/zebra-watchdog.service`](systemd/zebra-watchdog.service).
Manual installation on a host:

```bash
# Install the binary (built with: cargo build --release --locked -p zebra-watchdog)
install -m 755 target/release/zebra-watchdog /usr/local/bin/zebra-watchdog

# Optional: configure Sentry and overrides
mkdir -p /etc/zebra-watchdog
cat > /etc/zebra-watchdog/env <<'ENV'
SENTRY_DSN=https://<key>@<org>.ingest.sentry.io/<project>
SENTRY_ENVIRONMENT=zcashd-compat-mainnet
ENV
chmod 600 /etc/zebra-watchdog/env

# Install and start the service
cp deploy/watchdog/systemd/zebra-watchdog.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now zebra-watchdog

# Inspect
systemctl status zebra-watchdog
journalctl -u zebra-watchdog -f
```

The environment file is optional (`EnvironmentFile=-`): the service starts and
logs locally without it. It is operator-managed and is not overwritten by
deployments.

## Deploy workflow integration

[`.github/workflows/deploy-zcashd-compat.yml`](../../.github/workflows/deploy-zcashd-compat.yml)
deploys the watchdog alongside `zebrad`:

1. builds `zebrad` and `zebra-watchdog` (`cargo build --release --locked -p zebrad -p zebra-watchdog`),
2. uploads both binaries, [`sync-check.sh`](sync-check.sh), and the systemd unit,
3. installs both binaries and restarts `zebrad-compat`
   (only `zebrad` keeps a `.bak` rollback copy),
4. verifies the deployment with `/tmp/zebra-watchdog-sync-check.sh`
   (which runs `zebra-watchdog check`); on failure the workflow restores
   `/usr/local/bin/zebrad.bak` and fails,
5. on success, installs/refreshes the `zebra-watchdog` systemd unit and
   restarts the continuous watchdog service.

Rollback ownership stays with the workflow: the one-shot check only reports
pass/fail through its exit code.

### Manual one-shot check

Run the same verification manually on the host:

```bash
HEIGHT_MAX_DRIFT=10 SYNC_CHECK_TIMEOUT=1800 SYNC_CHECK_INTERVAL=15 \
  zebra-watchdog check
```

Or against custom endpoints:

```bash
zebra-watchdog check \
  --zebra-rpc-url http://127.0.0.1:8232 \
  --zebra-cookie-file /root/.cache/zebra/.cookie \
  --height-max-drift 10
```

## Development

```bash
# Type-check, lint, and test
cargo check -p zebra-watchdog --locked
cargo clippy -p zebra-watchdog --all-targets -- -D warnings
cargo test -p zebra-watchdog

# Build the release binary
cargo build --release --locked -p zebra-watchdog
```

Unit tests cover configuration defaults/overrides, JSON-RPC response parsing,
the readiness and drift predicates, the one-shot retry loop, and the
failure/recovery transition logic.

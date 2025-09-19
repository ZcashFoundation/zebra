# Zebra Health Endpoints

`zebrad` can serve two lightweight HTTP endpoints for liveness and readiness checks.
These endpoints are intended for Kubernetes probes and load balancers. They are
disabled by default and can be enabled via configuration.

## Endpoints

- `GET /healthy`
  - `200 OK`: process is up and has at least the configured number of recently
    live peers (default: 1)
  - `503 Service Unavailable`: not enough peers

- `GET /ready`
  - `200 OK`: node is near the network tip, the estimated lag is within the
    configured threshold (default: 2 blocks), and the most recent committed
    block is not stale
  - `503 Service Unavailable`: still syncing, lag exceeds the threshold, block
    commits are stale, or there are not enough recently live peers

## Configuration

Add a `health` section to your `zebrad.toml`:

```toml
[health]
listen_addr = "0.0.0.0:8080"      # enable server; omit to disable
min_connected_peers = 1            # /healthy threshold
ready_max_blocks_behind = 2        # /ready threshold
enforce_on_test_networks = false   # if false, /ready is always 200 on regtest/testnet
ready_max_tip_age = "5m"          # fail readiness if the last block is older than this
min_request_interval = "100ms"    # drop health requests faster than this interval
```

Config struct reference: [`components::health::Config`][health_config].

## Kubernetes Probes

Example Deployment probes:

```yaml
livenessProbe:
  httpGet:
    path: /healthy
    port: 8080
  initialDelaySeconds: 10
  periodSeconds: 10
readinessProbe:
  httpGet:
    path: /ready
    port: 8080
  initialDelaySeconds: 10
  periodSeconds: 5
```

## Security

- Endpoints are unauthenticated and return minimal plain text.
- Bind to an internal address and restrict exposure with network policies,
  firewall rules, or Service selectors.

## Notes

- Readiness combines a moving‑average “near tip” signal, a hard block‑lag cap,
  recent block age checks, and a minimum peer count.
- All health endpoints are globally rate limited; requests faster than
  `min_request_interval` receive `429 Too Many Requests`.
- Adjust thresholds based on your SLA and desired routing behavior.

[health_config]: https://docs.rs/zebrad/latest/zebrad/components/health/struct.Config.html

## Recent Improvements

### Network Disconnect Detection (Issue #4649)

The health endpoints now properly detect when Zebra loses network connectivity:

- When all sync lengths are zero (indicating no block downloads), the node is
  considered not ready, preventing false positives during network outages.
- Response bodies now include diagnostic information:
  - Peer counts are shown in success responses: `ok (peers=N)`
  - Failure reasons are specific: `insufficient peers: X < Y`, `lag=N blocks (max=M)`
- Regtest network always returns ready since no peers are expected: `ok (regtest)`

This prevents load balancers from routing traffic to disconnected nodes and
prevents the mempool from activating prematurely when peers are lost.

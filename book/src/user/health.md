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
  - `200 OK`: node is near the network tip, the estimated block lag is within
    the configured threshold (default: 2 blocks), and the tip is recent enough
    (default: 5 minutes)
  - `503 Service Unavailable`: still syncing, lag exceeds threshold, tip is too
    old, or insufficient peers

## Configuration

Add a `health` section to your `zebrad.toml`:

```toml
[health]
listen_addr = "0.0.0.0:8080"      # enable server; omit to disable
min_connected_peers = 1            # /healthy threshold
ready_max_blocks_behind = 2        # /ready block lag threshold
ready_max_tip_age = "5m"           # /ready tip age threshold (default: 5 minutes)
enforce_on_test_networks = false   # if false, /ready is always 200 on regtest/testnet
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

- Readiness combines a moving‑average “near tip” signal with a hard block‑lag cap.
- Adjust thresholds based on your SLA and desired routing behavior.

[health_config]: https://docs.rs/zebrad/latest/zebrad/components/health/struct.Config.html

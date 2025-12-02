# Zebra Grafana Dashboards

Pre-built dashboards for monitoring Zebra nodes.

## Quick Start

```bash
# From repository root
docker compose -f docker/docker-compose.observability.yml up -d
```

Access Grafana at http://localhost:3000 (admin/admin - you'll be prompted to change on first login).

## Dashboard Configuration

### Datasource

All dashboards use a provisioned Prometheus datasource with UID `zebra-prometheus`.
This is configured in `docker/observability/grafana/provisioning/datasources/prometheus.yml`.

### Rate Window Requirements

Dashboards use `rate()` functions for per-second metrics. The rate window must
contain at least 2 data points to calculate a rate.

| Scrape Interval | Minimum Rate Window |
|-----------------|---------------------|
| 500ms           | 1s                  |
| 15s (default)   | 30s                 |
| 30s             | 1m                  |

Current dashboards use `[1m]` windows, compatible with the default 15s scrape interval.

If you modify `docker/observability/prometheus/prometheus.yaml` scrape_interval, update dashboard queries accordingly.

### Job Label

The `$job` variable in dashboards is populated from Prometheus. The default job
name is `zebra` (configured in `docker/observability/prometheus/prometheus.yaml`).

## Dashboards

| Dashboard | Description |
|-----------|-------------|
| network_health.json | Peer connections, bandwidth |
| syncer.json | Sync progress, block downloads |
| mempool.json | Transaction pool metrics |
| peers.json | Peer connection details |
| block_verification.json | Block verification stats |
| checkpoint_verification.json | Checkpoint sync progress |
| transaction-verification.json | Transaction verification |
| network_messages.json | P2P protocol messages |
| errors.json | Error tracking |

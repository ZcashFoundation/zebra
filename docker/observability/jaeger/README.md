# Jaeger Distributed Tracing

Jaeger shows how requests flow through Zebra, where time is spent, and where errors occur.

## Why Jaeger?

While Prometheus metrics show **what** is happening (counters, gauges, histograms), Jaeger traces show **how** it's happening:

| Metrics (Prometheus) | Traces (Jaeger) |
|---------------------|-----------------|
| "RPC latency P95 is 500ms" | "This specific getblock call spent 400ms in state lookup" |
| "10 requests/second" | "Request X called service A → B → C with these timings" |
| "5% error rate" | "This error started in component Y and propagated to Z" |

Jaeger is used by other blockchain clients including Lighthouse, Reth, Hyperledger Besu, and Hyperledger Fabric for similar observability needs.

## Accessing Jaeger

Open http://localhost:16686 in your browser.

## Concepts

### Traces and Spans

- **Trace**: A complete request journey through the system (e.g., an RPC call from start to finish)
- **Span**: A single operation within a trace (e.g., "verify block", "read from database")
- **Parent-child relationships**: Spans form a tree showing how operations nest

### Span Kinds

Jaeger categorizes spans by their role:

| Kind | Description | Example in Zebra |
|------|-------------|------------------|
| **SERVER** | Entry point handling external requests | JSON-RPC endpoints (`getblock`, `getinfo`) |
| **INTERNAL** | Internal operations within the service | Block verification, state operations |
| **CLIENT** | Outgoing calls to other services | (Not currently used) |

This distinction is important for the Monitor tab (see below).

## Monitor Tab (Service Performance Monitoring)

The Monitor tab provides RED metrics (Rate, Errors, Duration) aggregated from traces.

### Understanding the Dashboard

```
┌─────────────────────────────────────────────────────────────────────┐
│  Service: zebra              Span Kind: [Internal ▼]               │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│   Latency (s)          Error rate (%)        Request rate (req/s)  │
│   ┌──────────┐         ┌──────────┐          ┌──────────┐          │
│   │    /\    │         │          │          │   ___    │          │
│   │   /  \   │         │  ──────  │          │  /   \   │          │
│   │  /    \  │         │          │          │ /     \  │          │
│   └──────────┘         └──────────┘          └──────────┘          │
│                                                                     │
├─────────────────────────────────────────────────────────────────────┤
│  Operations metrics                                                 │
│  ┌────────────────────────────────────────────────────────────────┐│
│  │ Name                      P95 Latency   Request rate   Error   ││
│  │ state                     10s           10.27 req/s    < 0.1%  ││
│  │ connector                 4.52s         0.24 req/s     < 0.1%  ││
│  │ checkpoint                95.88ms       6.69 req/s     < 0.1%  ││
│  │ best_tip_height           4.75ms        58.84 req/s    < 0.1%  ││
│  │ download_and_verify       2.32s         0.11 req/s     < 0.1%  ││
│  └────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────┘
```

### Columns Explained

| Column | Meaning | What to Look For |
|--------|---------|------------------|
| **Name** | Span/operation name | Group of related operations |
| **P95 Latency** | 95th percentile duration | High values indicate slow operations |
| **Request rate** | Operations per second | Throughput of each operation type |
| **Error rate** | Percentage of failures | Any value > 0% needs investigation |
| **Impact** | Relative contribution to overall latency | Focus optimization on high-impact operations |

### Span Kind Filter

The "Span Kind" dropdown filters which operations you see:

- **Internal**: All internal Zebra operations (default view)
  - Block verification, state operations, network handling
  - High volume during sync

- **Server**: External-facing request handlers
  - JSON-RPC endpoints (`getblock`, `getinfo`, `sendrawtransaction`)
  - Use this for RPC performance monitoring

### Interpreting Common Operations

| Operation | Description | Normal Latency |
|-----------|-------------|----------------|
| `state` | State database operations | 1-10s during sync |
| `connector` | Peer connection establishment | 1-5s |
| `dial` | Network dial attempts | 1-5s |
| `checkpoint` | Checkpoint verification | 50-200ms |
| `best_tip_height` | Chain tip queries | < 10ms |
| `download_and_verify` | Block download + verification | 1-5s |
| `block_commitment_is_valid_for_chain_history` | Block validation | 50-200ms |
| `rpc_request` | JSON-RPC handler (SERVER span) | Method-dependent |

## Search Tab

Use the Search tab to find specific traces.

### Finding Traces

1. **Service**: Select `zebra`
2. **Operation**: Choose a specific operation or "all"
3. **Tags**: Filter by attributes (e.g., `rpc.method=getblock`)
4. **Min/Max Duration**: Find slow or fast requests
5. **Limit**: Number of results (default: 20)

### Useful Search Queries

**Find slow RPC calls:**
```
Service: zebra
Operation: rpc_request
Min Duration: 1s
```

**Find failed operations:**
```
Service: zebra
Tags: error=true
```

**Find specific RPC method:**
```
Service: zebra
Tags: rpc.method=getblock
```

## Trace Detail View

Clicking a trace opens the detail view showing:

```
┌─────────────────────────────────────────────────────────────────────┐
│ Trace: abc123 (5 spans, 234ms)                                      │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│ ├─ rpc_request [SERVER] ─────────────────────────────── 234ms ────┤│
│ │  rpc.method: getblock                                            ││
│ │  rpc.system: jsonrpc                                             ││
│ │                                                                  ││
│ │  ├─ state::read_block ─────────────────────── 180ms ───────────┤││
│ │  │  block.height: 2000000                                       │││
│ │  │                                                              │││
│ │  │  ├─ db::get ───────────────────── 150ms ──────────────────┤│││
│ │  │  │  cf: blocks                                             ││││
│ │  │  └─────────────────────────────────────────────────────────┘│││
│ │  └─────────────────────────────────────────────────────────────┘││
│ └─────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────┘
```

### Reading the Waterfall

- **Horizontal bars**: Duration of each span
- **Nesting**: Parent-child relationships
- **Colors**: Different services/components
- **Tags**: Attributes attached to each span

### Span Attributes

| Attribute | Meaning |
|-----------|---------|
| `otel.kind` | Span type (server, internal) |
| `rpc.method` | JSON-RPC method name |
| `rpc.system` | Protocol (jsonrpc) |
| `otel.status_code` | ERROR on failure |
| `rpc.error_code` | JSON-RPC error code |
| `error.message` | Error description |

## Debugging with Traces

### Performance Issues

1. **Find slow traces**: Use Min Duration filter in Search
2. **Identify bottleneck**: Look for the longest span in the waterfall
3. **Check children**: See if parent is slow due to child operations
4. **Compare traces**: Compare fast vs slow traces for same operation

### Error Investigation

1. **Find failed traces**: Search with `error=true` tag
2. **Locate error span**: Look for spans with `otel.status_code=ERROR`
3. **Read error message**: Check `error.message` attribute
4. **Trace propagation**: See how error affected parent operations

### Example: Debugging Slow RPC

```
Problem: getblock calls are slow

1. Search:
   - Service: zebra
   - Operation: rpc_request
   - Tags: rpc.method=getblock
   - Min Duration: 500ms

2. Open a slow trace

3. Examine waterfall:
   - rpc_request: 800ms total
     └─ state::read_block: 750ms  ← Most time spent here
        └─ db::get: 700ms         ← Database is the bottleneck

4. Conclusion: Database read is slow, possibly due to:
   - Disk I/O
   - Large block size
   - Missing cache
```

## RPC Tracing

Zebra's JSON-RPC endpoints are instrumented with `SPAN_KIND_SERVER` spans, enabling:

- **Jaeger SPM**: View RPC metrics in the Monitor tab (select "Server" span kind)
- **Per-method analysis**: Filter by `rpc.method` to see specific endpoint performance
- **Error tracking**: Failed RPC calls have `otel.status_code=ERROR`

### RPC Span Attributes

Each RPC request includes:

| Attribute | Example | Description |
|-----------|---------|-------------|
| `otel.kind` | `server` | Marks as server-side handler |
| `rpc.method` | `getblock` | JSON-RPC method name |
| `rpc.system` | `jsonrpc` | Protocol identifier |
| `otel.status_code` | `ERROR` | Present on failure |
| `rpc.error_code` | `-8` | JSON-RPC error code |

## Compare Tab

Use Compare to diff two traces:

1. Select two trace IDs
2. View side-by-side comparison
3. Identify differences in timing or structure

Useful for:
- Comparing fast vs slow versions of same request
- Before/after optimization comparisons
- Debugging intermittent issues

## System Architecture Tab

Shows service dependencies based on trace data:

- Nodes: Services (currently just `zebra`)
- Edges: Communication between services
- Useful when Zebra calls external services

## Configuration

Jaeger v2 configuration is in `config.yaml`:

```yaml
# Key settings
receivers:
  otlp:
    protocols:
      http:
        endpoint: 0.0.0.0:4318  # OTLP HTTP (used by Zebra)

processors:
  batch: {}  # Batches spans for efficiency

connectors:
  spanmetrics:  # Generates Prometheus metrics from spans
    namespace: traces.spanmetrics

exporters:
  prometheus:
    endpoint: 0.0.0.0:8889  # Spanmetrics for Prometheus
```

### Spanmetrics

Jaeger automatically generates Prometheus metrics from spans:

```bash
# View spanmetrics
curl -s http://localhost:8889/metrics | grep traces_spanmetrics
```

These power the Monitor tab and can be scraped by Prometheus for Grafana dashboards.

## Usage Tips

### During Initial Sync

- **Reduce sampling** to 1-10% to avoid overwhelming Jaeger
- **Focus on errors** rather than complete traces
- **Use metrics** (Prometheus) for high-level sync progress

### Steady State Operation

- **Increase sampling** to 10-50% for better visibility
- **Monitor RPC latency** in the Monitor tab (Server spans)
- **Set up alerts** for high error rates

### Debugging Sessions

- **Set sampling to 100%** temporarily
- **Use specific searches** to find relevant traces
- **Compare traces** to identify anomalies
- **Remember to reduce sampling** after debugging

## Troubleshooting

### No traces appearing

1. Check OTLP endpoint: `OTEL_EXPORTER_OTLP_ENDPOINT=http://jaeger:4318`
2. Verify Jaeger health: http://localhost:16686
3. Check Jaeger logs: `docker compose logs jaeger`

### Monitor tab shows "No Data"

1. Ensure spanmetrics connector is configured
2. Wait for traces to accumulate (needs a few minutes)
3. Select the correct Span Kind filter

### High memory usage

1. Reduce sampling: `OTEL_TRACES_SAMPLER_ARG=10`
2. Restart Jaeger to clear memory
3. Consider using external storage for production

## Further Reading

- [Jaeger Documentation](https://www.jaegertracing.io/docs/)
- [OpenTelemetry Tracing Concepts](https://opentelemetry.io/docs/concepts/signals/traces/)
- [Jaeger v2 Architecture](https://www.jaegertracing.io/docs/2.0/architecture/)

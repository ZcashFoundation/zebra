---
status: proposed
date: 2026-11-13
story: Define each telemetry signal once and serve Prometheus, Sentry, and OpenTelemetry from it (#11629, #11631, #11641)
---

# Define telemetry signals once and give every backend the same signals

## Context and Problem Statement

At commit `1ab1f8db2`, Zebra sends telemetry to three backends, and each one is maintained separately:

- **Prometheus.** Call sites across `zebra-consensus`, `zebra-network`, `zebra-state`, `zebra-rpc`, and `zebrad` record counters, gauges, and histograms through the [`metrics`](https://docs.rs/metrics) facade. `metrics-exporter-prometheus` serves them when `[metrics] endpoint_addr` is set. Operators and the dashboards and alert rules in `docker/observability/` (also copied into the z3 stack) consume these.
- **Sentry.** A second recorder, combined with the Prometheus recorder by `metrics_util::layers::FanoutBuilder` (`zebrad/src/components/metrics.rs`), forwards only names on hand-maintained lists in `zebrad/src/sentry/periodic.rs` and `zebrad/src/sentry/distributions.rs`. Some signals skip the facade and call the Sentry SDK directly. Errors, panics, release health, and selected logs also go to Sentry.
- **OpenTelemetry.** Release builds include the `opentelemetry` feature, which exports `tracing` spans over OTLP. No metrics go to OpenTelemetry.

Nothing records what a signal is: its name, kind, unit, labels, and meaning live only at the call site. So each backend has drifted from the code and from the others. These are the cases observed on 2026-11-12, at commit `1ab1f8db2`:

| Drift | Effect |
| --- | --- |
| The `ZebraHighErrorRate` rule in `docker/observability/prometheus/rules/zebra_alerts.yml` queries `zebra_errors_total`, which no code records. | The alert can never fire. |
| Dashboards query `process_resident_memory_bytes`, `process_open_fds`, `process_cpu_seconds_total`, and other `process_*` series. Zebra exports none of them. | Resource panels are always empty. |
| Dashboards query `rpc_request_duration_seconds_bucket`, and alert rules query it and `zcash_net_peer_handshake_duration_seconds_bucket`. Zebra never configures histogram buckets, so `metrics-exporter-prometheus` renders every histogram as a summary with quantiles and no `_bucket` series. | Latency panels are empty, the latency alerts can never fire, and per-node quantiles can't be aggregated across nodes. |
| About a dozen other dashboard queries name series that no code records, such as `state_q_max`, `sync_pending_blocks_len`, and `zebra_sighash_error_sapling_spend`. | More empty panels. |
| The observability READMEs in Zebra and z3 tell operators to check the stack by querying `zcash_state_tip_height`, which no code records. | The documented check always returns nothing. |
| z3 vendors the dashboards and alert rules of the Zebra release it pins: at z3 `e84ce9f` they are byte-identical to Zebra `v6.2.3`, its default image. Checked against `v6.2.3`'s code, they reference the same missing series as above. | Every z3 user, the one public deployment with metrics on by default, inherits the broken panels and alerts. Version pinning itself works: z3's copies of three dashboards differ from Zebra `main` only because `v7.0.0` removed per-peer labels. |
| The first version of the process memory gauges, on an unmerged branch, sampled them inside the Sentry flush thread. The branch also names resident memory `process.memory.rss_bytes`, while the dashboards expect `process_resident_memory_bytes`. | Prometheus users wouldn't have received them, and resident memory would have had two names. |
| `zcash.chain.tip.block_delay_seconds` is recorded twice at one call site: a `metrics` histogram for Prometheus, and a direct Sentry distribution that adds the block height. | Two code paths for one signal. |
| Sentry receives about ten hand-picked metrics, out of more than 170 names that Prometheus receives. | A Sentry user can't see most of what a Prometheus user can, and the list grows only when someone remembers it. |
| Sync stalls and node starts exist only as log lines, so Sentry alerts on them have to match log text. | Prometheus users can't alert on these conditions, and rewording a log message silently breaks an alert. |

Drift runs in every direction: signals added for Sentry miss Prometheus users, signals in Prometheus miss Sentry users, and dashboards miss both. The question is where a signal is defined, how every backend derives from that one definition, and how dashboards and alert rules are kept consistent with it.

## Priorities & Constraints

- **Backends are peers.** Prometheus, Sentry, and OpenTelemetry are all operator-facing, and an operator may run any combination of them. Prometheus support and Sentry support have both shipped in release builds for years and are documented in the book. The z3 stack turns on the metrics endpoint by default and documents sending traces over OTLP to Jaeger. ZF's own fleets happen to alert from Sentry, but that is one operator's choice, not a property of the backend. An operator must be able to pick a backend knowing they get the same signals as anyone else, so a signal added for one backend and forgotten in the others is a defect.
- **Existing Prometheus names are a public contract.** Operators' dashboards and alert rules depend on them. Renames need a deprecation period.
- **Backends differ in volume and cardinality.** Sentry stores every value it receives. The SDK queues each `capture()` call, batches 100 items or 5 seconds into an envelope, and keeps at most 30 envelopes in its transport queue, which overflowed during an initial sync. Events, logs, sessions, and metrics share that queue. Prometheus keeps one series per label set, so a label like the block height creates unbounded series. Sentry attributes can carry it.
- **Dependency rules.** Lower crates record metrics and must not depend on `zebrad`. `zebra-chain` is sync-only.
- **Reuse before building.** The `metrics` facade already has metadata (`describe_counter!` and similar, with unit and description), recorder composition (`metrics-util` layers), and a test recorder (`DebuggingRecorder`). A new mechanism has to be justified against them.
- **Small public PRs**, each useful on its own.

## Considered Options

1. **The `metrics` facade stays the source, with a described catalog and per-backend policies in `zebrad`.**
2. **OpenTelemetry metrics become the source**, with Prometheus, Sentry, and OTLP as exporters.
3. **A Zebra telemetry registry or macro crate** that emits to several sinks.
4. **`tracing` events become the source**, with layers deriving metrics.

### Pros and Cons of the Options

#### Option 1: `metrics` facade, described catalog, per-backend policies

- Good, because every call site, the Prometheus exporter, and all existing names stay as they are. The change is additive.
- Good, because `describe_*!` is the facade's own place for a metric's unit and description. The Prometheus exporter already turns descriptions into `# HELP` lines, so operators get documentation at the endpoint.
- Good, because each backend's policy is a `Recorder` or `metrics-util` layer in `zebrad`, composed by the existing fanout. Lower crates stay unaware of backends.
- Good, because comparable Rust nodes use the same facade boundary: Reth records through it with a derive macro that describes each metric, and Zakura replaced its Prometheus exporter behind it without changing names.
- Bad, because `describe_*!` carries no labels, cardinality class, or backend policy. These have to live in label tables and conventions, enforced by tests rather than by types.
- Bad, because a call site can still record a name nobody described. A test has to catch it.

#### Option 2: OpenTelemetry metrics as the source

- Good, because OTLP is the cross-vendor standard and OpenTelemetry semantic conventions give ready names for process and runtime metrics.
- Bad, because Sentry doesn't accept OTLP metrics. Sentry's OTLP endpoint and Relay's router take only `/v1/traces` and `/v1/logs`. Zebra would still need a custom Sentry exporter, so one of the three paths stays custom.
- Bad, because the Prometheus path gets weaker. The Rust pull exporter, `opentelemetry-prometheus`, is rated Beta while the metrics SDK and OTLP exporter are Stable, and the project points new users to OTLP push instead, which needs a Collector or Prometheus's OTLP receiver. Either route applies OpenTelemetry's name translation (unit and `_total` suffixes), which renames existing series unless every name is pinned with views or translation settings.
- Bad, because it rewrites every call site in five crates, and Prometheus and Sentry users get nothing they lack today.

#### Option 3: A Zebra telemetry registry or macro crate

- Good, because a definition could carry everything: kind, unit, labels and their cardinality class, and backend policy, checked at compile time.
- Bad, because it is a new published crate at the bottom of the dependency graph, and a second API beside the facade that every contributor has to learn. The existing `metrics-derive` crate (Reth's) carries descriptions but not units, labels, or policy, so it wouldn't cover the gap alone.
- Bad, because the gain over Option 1 is the compile-time check of labels and policy. The drift observed today is between code and dashboards, which a type system can't see, and tests on a catalog cover it.
- Neutral: Option 1 doesn't rule this out. If the label tables grow hard to keep consistent, the catalog can move behind a macro without changing names or backends.

#### Option 4: `tracing` events as the source

- Good, because logs and metrics would share one call. `tracing-opentelemetry`'s `MetricsLayer` derives metrics from event fields with `counter.` or `histogram.` prefixes.
- Bad, because metric identity becomes a naming convention on log fields. Kind and unit are encoded in field names, there's no description, and filtering a log level or target would drop metrics.
- Bad, because the hot paths that record metrics today would pay for event dispatch through every subscriber layer.
- Bad, because it only bridges to OpenTelemetry metrics, so it inherits Option 2's Sentry and Prometheus problems.

## Decision Outcome

Chosen option: **Option 1**. The `metrics` facade stays the single place where values are recorded. Each crate describes the metrics it records in one module, the union of those descriptions is the **signal catalog**, and every backend, dashboard, and alert rule is derived from or checked against that catalog.

One rule governs every backend: **every recorded signal reaches every enabled backend.** A backend's policy may change how a signal is shaped for it, but never which signals it receives. Shaping covers these differences:

- **Aggregation.** Prometheus serves running totals and latest values at each scrape. Sentry receives a counter's increase and a gauge's value only when they change, plus a periodic gauge refresh (part 3).
- **Histogram form.** Prometheus serves buckets or quantiles. Sentry receives a bounded number of individual values.
- **Attributes.** Prometheus drops per-event values that would become series. Sentry keeps them.
- **Volume limits.** Sentry has a per-flush budget and a per-metric label-set cap. Anything a limit leaves out is counted, so the reduction is visible in every backend.

The parity test enforces the rule (part 5), so it doesn't depend on contributors remembering each backend.

Phases 0 to 3 of the migration plan implement parts 1 to 3, the drift checks of part 5, and the performance work of part 8, on commits based on `1ab1f8db2`, starting with #12434. The parts below describe that implementation. Work it leaves open is marked in the part that needs it and listed in the migration plan.

The design has eight parts.

### 1. Definition: one `metrics` module per crate

Each library crate that records metrics has a `metrics` module (`zebra-consensus`, `zebra-network`, `zebra-rpc`, and `zebra-state`). `zebrad` keeps its own in `zebrad/src/components/metrics/catalog.rs`. Each module has:

- a `pub fn describe()` that calls `metrics::describe_counter!`, `describe_gauge!`, or `describe_histogram!` for each name, with its `metrics::Unit` and a one-line description;
- a `pub const LABELS: &[(&str, &[&str])]` table listing each labelled metric's label keys, with a doc comment naming each label's allowed values, for example "`reason`: one of `queue_full`, `no_serving_peer`".

Call sites keep their string literal names. There are no name constants. Existing call sites stay unchanged, each name stays searchable where it is recorded, and the call-site check (part 5) reads literal names directly. A name built another way, such as `zebrad.build.info` with `concat!`, isn't seen by that check. The parity test still covers it.

`describe_all()` in `zebrad/src/components/metrics.rs` calls every crate's `describe()` and describes the `metrics-process` metrics. `zebrad` calls it immediately after it installs the global recorder, before any component records. Descriptions issued while no recorder is installed go to the `metrics` crate's no-op recorder and are lost. If no backend is enabled, there is nothing to describe to.

The catalog is the set of described metrics. A metric's **kind** comes from which `describe_*!` macro was used, and its **unit** and **description** from the macro's arguments. Every metric has a unit and a description, which a test checks. Its **labels** come from the `LABELS` table. Each label key has one of two **cardinality classes**:

- **Labels** have a small, fixed set of values, such as an enum, a result, a network kind, or a protocol command. Every backend receives them.
- **Event attributes** may take a new value on every record, such as a block height. Their keys start with the reserved prefix `event.`, for example `event.chain.height`, so the call site itself says which class a key is in, and no list has to be kept in sync across crates. Event attributes are allowed only on histograms recorded at most about once per block.

Label values follow these rules, because a label value reaches every backend and every operator who reads it:

- **No peer-controlled values.** A value taken from peer input is mapped to a fixed set first. For example, `zcash.net.peers.missing_services` labels a rejected peer with `missing_service="NODE_NETWORK"`, the required service it lacks, instead of the service bits it sent.
- **No local paths or host details.** The peer cache file path is not a label.
- **Operator configuration is allowed when it is bounded.** The `seed` label on `zcash.net.peers.initial` names a DNS seeder host from the node's configuration. It takes as many values as there are configured seeders, and the crate's `LABELS` documentation says so.

An event attribute travels as an ordinary label in the `metrics::Key` from the call site. Each backend splits it off before the key reaches a registry (`zebrad/src/components/metrics/event_labels.rs`):

- The Prometheus recorder sits behind `StripEventLabelsLayer`, which drops event attributes, so a new event attribute added in any crate can never become a Prometheus series.
- The Sentry policy keeps them beside each histogram value, and sends them without the `event.` prefix (`chain.height`, the name Sentry dashboards already use). Integer values are sent as integers, so Sentry queries can compare them. Counters and gauges drop them, because the policy keeps their values for the life of the process.

Splitting copies the key's labels on every record. That cost (part 8) is the reason event attributes are limited to once-per-block histograms.

New names follow the existing dot-separated prefixes and end in their unit (`_seconds`, `_bytes`). Process metrics use the Prometheus process collector's names (`process_resident_memory_bytes` and so on), because operators' dashboards, including Zebra's own, already query them.

### 2. Collection: sampling never depends on a backend

Values that are sampled rather than recorded when something happens are sampled by the module that owns them, on a fixed interval, whenever at least one backend is enabled, and never when none is (part 8). No backend's code computes a value. This prevents the memory-gauge drift: a backend's flush reads the latest value like any other gauge.

Process-wide values, such as memory, open file descriptors, and CPU time, come from the `metrics-process` crate (2.4.3), which records them through the facade under the Prometheus process-collector names. `zebrad` samples them once when a recorder is installed, then every 15 seconds on a dedicated `process-metrics` thread. It starts no sampler when no recorder is installed.

The state samples its RocksDB properties every 30 seconds in a Tokio task started in `zebra-state/src/service.rs`. That task runs whether or not a backend is enabled, and runs on the async runtime, so it doesn't yet meet this part or requirements 1 and 6 of part 8. Moving it is open.

### 3. Export: one policy per backend, in `zebrad`

Backend code lives in `zebrad`: the recorders, layers, and policies in `zebrad/src/components/metrics.rs` and its submodules, and the Sentry client and its metrics adapter in `zebrad/src/sentry.rs` and `zebrad/src/sentry/metrics_adapter.rs`. Each backend is a `Recorder`, wrapped in `metrics-util` layers, and combined in the existing fanout. Each policy decides only how to shape signals for its backend, and emits plain values to a thin adapter that talks to the backend's library. The Sentry policy (`sentry_policy.rs`) hands each flush to a closure as a batch of plain values, so tests capture the batch instead of calling the SDK.

| | Prometheus | Sentry | OpenTelemetry |
| --- | --- | --- | --- |
| Which metrics | All recorded metrics. | All recorded metrics, with at most 256 label sets per metric name. | All recorded metrics, today by a Collector scraping the Prometheus endpoint with its `prometheus` receiver. |
| Counters | Cumulative, as today. | The increase since the counter was last sent, for each label set that increased. Nothing for a label set that didn't. Counters set with `absolute()`, such as `process_cpu_seconds_total`, send their increase too, because the policy keeps the previous total instead of resetting. | Through the scrape. |
| Gauges | Latest value at scrape. | The latest value, for each label set whose value changed since it was last sent, and every 5 flushes (5 minutes) for one that didn't. A gauge that was never set, or isn't finite, isn't sent. | Through the scrape. |
| Histograms | Real Prometheus histograms for the metrics in `HISTOGRAM_BUCKETS` (`zebrad/src/components/metrics.rs`), so they aggregate across nodes. Today that is `rpc.request.duration_seconds` and `zcash.net.peer.handshake.duration_seconds`. The rest stay summaries. | Distributions. Each label set sends at most 20 values per 60-second flush, taken at evenly spaced positions among that interval's values, so the values sent span the whole interval. Low-rate histograms, such as the tip block delay, stay under the cap and are sent in full. | Through the scrape. |
| Event attributes | Dropped by `StripEventLabelsLayer`, because each value would become a series. | Kept on distributions, without the `event.` prefix. Dropped from counters and gauges. | Dropped (through the scrape). |
| Context | Prometheus target labels. | Network, network upgrade at the tip, `chain.near_tip`, and `process.uptime_seconds`, added to every metric when it is sent (part 3, node context). | Collector resource attributes. |
| Names | Dots become underscores, as today. Unit suffixes stay off, so names don't change. | Dotted names, as today. | Prometheus names. |

The rows differ only where a backend can't represent a signal the same way, or where Sentry's volume limits apply. Event attributes are the one place where Prometheus receives less: it can't store a per-block value as a label. The signal itself still reaches Prometheus, and the catalog documents the difference.

**Recording.** The Sentry policy keeps one atomic value per label set in a sharded `metrics_util` registry with its own storage type. Recording a counter or gauge updates atomics, and recording a histogram appends to an atomic bucket. Nothing on the recording thread calls the Sentry SDK. The flush thread turns what was recorded into one batch every 60 seconds.

**Volume.** Sentry receives every recorded metric instead of a hand-picked list, because a list of what one operator alerts on is exactly the drift this decision removes. Volume stays bounded by shape and by these limits, not by selection:

- **Per-flush budget.** One flush sends at most 1,500 metrics: 15 envelopes of 100, half of the SDK's 30-envelope transport queue. That leaves room for events, logs, and sessions even while the transport sends nothing. When more are due, counters go first, then gauges that changed, then gauges due for a refresh, then histogram values. Counters and gauges that don't fit stay due and take turns in later flushes, so an increase is delayed, not lost. Histogram values that don't fit are dropped, at evenly spaced positions across every label set.
- **Pacing.** The adapter sends a batch in parts of 100 metrics, one second apart. A responsive transport then sends each envelope before the next one is queued, so a flush holds about one place in the queue at a time, and a full flush takes about 15 seconds.
- **Backpressure.** Before a flush, the adapter checks whether the transport has sent what earlier flushes queued, waiting up to 2 seconds. If it hasn't, the flush sends nothing: counters and gauges stay due, and histogram values are dropped. Metrics never take more than their share of a slow or stalled transport's queue.
- **Label-set cap.** The policy keeps at most 256 label sets per metric name, and doesn't keep the values recorded for any others. A label that keeps taking new values then can't grow the policy's memory or volume without bound. The cap applies to Sentry only: Prometheus keeps every label set.

**Drop accounting.** The `zebrad.sentry.metrics.dropped.total` counter counts values the policy left out, with a `reason` label:

- `flush_budget`: due values that didn't fit in a flush. This includes counters and gauges that a later flush sends.
- `transport_backlog`: due values in a flush that sent nothing because the transport was still busy.
- `label_set_cap`: values recorded for label sets over the cap.

The policy records this counter through the facade, so every backend receives it. It also logs a warning when drops start, then at most once an hour. Two reductions aren't counted:

- Histogram values left out by the 20-value cap per label set are sampling by design, not drops.
- Metrics the SDK discards after the policy hands them over, such as on a rate limit or an HTTP failure, are invisible to Zebra.

**Lifecycle.** The Sentry recorder exists only for an enabled client. Creating it starts the flush thread and a transport check thread, after the tracing subscriber is installed, so a failure to start them is logged. If they can't start, no Sentry recorder is installed, because without flushes the policy would keep every histogram value it records. At shutdown or panic, Zebra queues the release health session update first, then the due metrics without pacing, so metrics can't take the session's place in the transport queue.

**Build info.** `zebrad.build.info` is recorded whenever any backend is installed, including Sentry alone. It is recorded right after the descriptions, before Zebra logs that the metrics endpoint is open.

**Evidence.** The parity and volume tests run the Sentry policy over the whole catalog (part 5). Two Regtest runs with both backends on gave these results:

- Prometheus and Sentry carried the same recorded metrics.
- For all 94 counter label sets, the sum of the increases sent to Sentry equalled the Prometheus total.
- The largest flush sent 309 metrics in 4 envelopes, and nothing was dropped.
- The removed labels were absent.
- A Sentry-only node sent its build info.
- No telemetry threads ran with both backends off.

These runs didn't exercise the tip block delay (Regtest block timestamps don't produce it), `missing_service`, real Sentry ingestion and rate limits, a long initial sync, or network upgrades after Canopy.

**Exclusion.** Zebra has no setting to exclude names. If operators need less volume in one backend, the setting is one backend-neutral list that applies to every enabled backend, with per-backend overrides only where a backend's cost model needs them. Exclusion is the operator's choice, not a Zebra default. It is open in the migration plan.

**Tip block delay.** The tip block delay is one call in `record_tip_block_delay` (`zebrad/src/components/sync/progress.rs`): a histogram with an `event.chain.height` event attribute and a `chain.network_upgrade` label. The network upgrade comes from the block's own height, because the node context's tip height lags behind a new tip. Prometheus drops the event attribute and Sentry keeps it, so the direct Sentry distribution and its second code path are gone, and the Sentry policy needs no knowledge of the chain.

**Node context.** Chain context that several backends attach moves from the Sentry module into a backend-neutral node context (`zebrad/src/components/metrics/node_context.rs`). The context holds the network, the tip height, and whether the node is near the tip. The sync progress task writes it in every build, not only builds with the `sentry` feature. Backends read atomics rather than the chain tip channel, because Sentry calls its hooks from inside tracing, where an instrumented read would re-enter the tracing layers.

Sentry events and logs get the tip height and network upgrade. Metrics get the network upgrade and `chain.near_tip`, but not the tip height, because a block timing value is often for a block above the tip. Sentry adds these attributes in `before_send_metric`, when the flush sends the batch. So `chain.near_tip` describes the node when the batch was sent, usually within one flush interval of when the value was recorded, and a value recorded just before the node reached the tip can be marked as near the tip.

On the `next` branch, block phase timings are recorded only near the tip. With `chain.near_tip` on every metric, queries choose which values to count, and that gate can be removed so that nothing is discarded at the source.

OpenTelemetry receives metrics through the scrape today. It gets the same catalog, but needs a Collector to read it. A native OTLP metrics adapter, another recorder in the fanout, is phase 6 of the migration, so OpenTelemetry users get push delivery without extra infrastructure.

### 4. Traces and logs follow the same rule

Spans and log events are recorded once through `tracing`, and each backend's layer reads the same events. What each backend receives is decided by one shared classification, not by per-backend lists:

- Every backend that takes logs receives WARN and ERROR events, plus INFO events from a shared list of operator-relevant targets, such as the once-a-minute sync progress log. That list is backend-neutral. Sentry Logs uses it today. If an OpenTelemetry logs exporter is added, it uses the same list.
- Errors and panics go to every backend that represents them: Sentry issues today, and OpenTelemetry error events on spans.
- Release health sessions have no equivalent in Prometheus or OpenTelemetry. Their content is also available as metrics: start time, panics, and uptime.
- Spans go only to a backend that sends traces. Zebra doesn't send Sentry traces, so the Sentry tracing layer has a span filter that builds no Sentry spans. The OpenTelemetry layer consumes every span, and its sampler decides which ones are exported.

A condition operators need to alert on is never available only as a log or a Sentry issue. It also has a counter (part 6), so Prometheus users can alert on it too.

### 5. Drift checks: the catalog is a generated, checked file

The catalog is checked in at `docker/observability/metrics-catalog.json`, beside the dashboards and alert rules it is checked against. It held 192 metrics when it was first generated. Each entry has the name, kind, unit, description, label keys, and the Prometheus type and series names the exporter renders.

The `catalog_file_is_current` test in `zebrad/src/components/metrics/tests.rs` generates it. The test installs a small recorder that keeps every `describe_*` call, calls `describe_all()`, merges in each crate's `LABELS`, and fails if the checked-in file differs. `metrics-util`'s `DebuggingRecorder` doesn't fit, because its snapshots leave out metrics that are described but never recorded. Every change to a signal shows up in review as a catalog diff. When the file is stale, the test prints the command that regenerates it:

```sh
ZEBRA_UPDATE_METRICS_CATALOG=1 cargo nextest run -p zebrad catalog_file_is_current
```

The repository's path filter (`.github/path-filters.yml`) runs the unit tests when anything under `docker/observability/` changes, so a dashboard-only or rule-only change is checked too. A book metrics reference generated from the catalog is open (phase 5).

Five checks run against the catalog in CI:

1. **Backend parity.** `every_catalog_metric_reaches_every_backend` (`zebrad/src/components/metrics/tests/backends.rs`) records one value for every catalog metric, with every label key, through the fanout of the Prometheus recorder and the Sentry policy, as `zebrad` installs them. It checks that the rendered Prometheus output has every series the catalog lists, and that the next Sentry flush has a value of the right type for every metric. It also checks that no event attribute reaches Prometheus, and that Sentry receives event attributes without the prefix. `sentry_only_install_sends_the_build_info` checks the Sentry-only case. A signal that reaches only some backends fails CI, whichever backend it was added for.
2. **Sentry volume.** Two tests in the same module check the limits of part 3. The first simulates a busy node: every label set of every catalog metric within one interval, with label value counts based on each crate's `LABELS` documentation and observed connection-state cardinality, and every histogram above its cap. One flush must send every counter and gauge, and values from every histogram label set, within the budget. The second checks that a flush stays within the budget for any number of label sets, and counts every value it leaves out.
3. **Call sites.** `call_sites_record_cataloged_metrics` (`tests/call_sites.rs`) scans the workspace's non-test sources for `counter!`, `gauge!`, and `histogram!` calls. It fails if a literal name isn't in the catalog, is recorded as a different kind, or uses a literal label key missing from that metric's `LABELS` entry.
4. **Dashboards and alert rules.** `queries_use_exported_series` (`tests/queries.rs`) extracts series names from the PromQL in `docker/observability/grafana/dashboards/*.json`, the `expr:` lines in `docker/observability/prometheus/rules/*.yml`, and the example queries in `docker/observability/README.md`. It fails on any name that isn't a series the catalog says Zebra exports. Bucketed histograms export `_bucket`, `_sum`, and `_count`, and summaries export the bare name (with a `quantile` label), `_sum`, and `_count`. So a `_bucket` query matches only a bucketed histogram, and a quantile query only a summary. A short allowlist covers series Zebra doesn't produce: `up`, and Jaeger's `traces_spanmetrics_` span metrics.
5. **Catalog consistency and policies.** Building the catalog fails if a name is described twice, a `LABELS` entry names an undescribed metric, or `HISTOGRAM_BUCKETS` names something that isn't a described histogram. Other tests check that every metric has a unit and a description, and that `event.` keys appear only on histograms.

**The `next` branch.** These checks run on `next` too. `next` records metrics that `main` doesn't: the block phase timings, the block template and submission metrics, and the mined block announcement metrics. Its dashboards and alert rules also query the RPC and handshake latencies as quantiles. The sync from `main` to `next` must catalog those metrics in the crates' `describe()` functions and `LABELS` tables, and rewrite those queries to `histogram_quantile` over the `_bucket` series, or `next` fails the checks.

**External consumers.** z3 already copies the dashboards and rules from the Zebra release it pins, so fixing them in Zebra reaches z3 at its next version bump, and z3 needs no check of its own while it keeps them unmodified. Copies that are changed, and any operator's Sentry dashboards and alerts, including ZF's, check their queries against the catalog JSON of the Zebra release they target, using the same mapping. Publishing the file with each release is phase 5; those repositories run the check.

Checking was chosen over generating dashboards and rules from the catalog. Generation (for example jsonnet mixins) pays off only with a regenerate-and-diff gate, and it would replace hand-edited Grafana JSON that operators copy. The checks catch the drift observed today at a fraction of the cost.

### 6. Logs as signals: alerts target metrics, not log text

When an alert or dashboard needs to know that something happened, the call site that logs it also increments a counter in the catalog: for example a sync stall, or a node start (the build info series plus `process_start_time_seconds`). Non-finalized fork evictions already follow this pattern with `state.memory.fork.evicted.count`, and `ZebraHighErrorRate` now fires on `sync.block.error.count`. The log line stays for context. Zebra's shipped alert rules, and the alerts it documents for any backend, use these counters rather than log text, so rewording a message can't silently break an alert, and a Prometheus user can alert on the same conditions as a Sentry user.

### 7. Renames and deprecation

Existing exported names don't change as a side effect of this work. When a name must change:

1. The Prometheus policy records the metric under both names for at least one minor release, using a rename table in the policy module. No rename has needed one yet, so the table doesn't exist.
2. The release adds a `Deprecated` change fragment naming the old and new series.
3. The following release removes the old name with a `Removed` fragment.

Turning an existing summary into a bucketed histogram is also a breaking change, even though the name stays: its `quantile` series disappear, and the exporter can't serve both forms under one name. It follows the same steps, with the `Deprecated` fragment one release before the buckets are added. Exception: a metric whose only queries in the repository use `_bucket`, which returned nothing before, may be bucketed directly with a `Changed` fragment. That covered `rpc.request.duration_seconds` and `zcash.net.peer.handshake.duration_seconds`.

A label that breaks the label rules of part 1 is the defect itself, so it is replaced or removed directly with a `Changed` fragment that tells operators to update their selectors. That covered the `remote_services` label on `zcash.net.peers.missing_services`, now `missing_service`, and the `cache` label on `zcash.net.peers.initial` and `zcash.net.peers.cache`.

Dashboard and rule queries for series that never existed, like those in the table above, are fixed directly. No operator received data from them, so there's nothing to deprecate.

### 8. Performance: off costs nothing, on costs the least

Parity must not mean that running several backends slows the node, or that a backend's overhead distorts the values it reports. Release builds compile all three backends in, so what matters to operators is the cost when a backend is compiled in but turned off, and the cost of each one turned on.

**Before this work, at commit `1ab1f8db2`:**

| Mechanism | Compiled in, turned off | Turned on |
| --- | --- | --- |
| `metrics` call sites | Negligible for fixed names: a thread-local check and one atomic load, then a no-op. Call sites that build labels with `format!` or `to_string` still allocate, because arguments are evaluated first. | Per record, for each backend: a registry lookup and a new handle allocation, unless the caller keeps the handle. Only one place in the code keeps handles. |
| Prometheus | None: nothing is installed without `endpoint_addr`. | Low: atomics per record, plus one exporter thread. A scrape never blocks recording. |
| Sentry metrics | None: installed only with an enabled client. | Low for the current list. Listed counters and gauges take one global mutex per record, and listed histograms call the SDK on the recording thread. |
| Sentry tracing layer | None. | **Medium to high.** Zebra sets no span filter, so every INFO span becomes a Sentry span, and every poll of an instrumented future forks a hub and clones a scope. That includes `msg_as_req`, a span for each inbound peer message. No `traces_sample_rate` is set, so these spans are built and then dropped unsent. |
| OpenTelemetry tracing | None: no SDK objects are created without an endpoint. | Medium: attributes are built for every span before sampling decides, so a 1% sample rate still pays for 100% of span creation. Export runs on its own thread. |
| Several at once | | Costs add, plus the fanout's own cost: it registers each record with every child and allocates for the combined handle. |

Costs paid while everything was off: label strings built on peer-message paths (`connection.rs` formatted a state name twice per message), the state sampling RocksDB properties every 30 seconds, and two Sentry threads started when `SENTRY_DSN` is set but the client is disabled.

**Changes made.** The Sentry tracing layer has `span_filter(|_| false)`, so it builds no Sentry spans (#12434). Sentry's session rotation and metrics flush threads start only for an enabled client. The connection-state gauge uses `Copy` label keys made of static strings, and caches one gauge handle per label per thread (`zebra-network/src/peer/connection/state_label.rs`), so a peer message formats nothing. The RocksDB sampler is unchanged.

**Observer effects.** Before the span filter, the per-poll Sentry span work fell inside the peer handshake duration and message handling, so those timings reported part of Sentry's own overhead. RPC request durations may include OpenTelemetry span work, depending on middleware order. Batch verifier durations stop the clock before recording, so recording isn't included, but they include time queued for blocking threads, which telemetry threads compete for. These estimates come from reading the code, not from measurement.

**Requirements.** Each requirement is part of the decision. Where the implementation doesn't meet one yet, the requirement says so, and the migration plan lists the work.

1. **A disabled backend installs nothing:** no recorder, layer, thread, or sampler. With every backend off, telemetry costs one atomic load per fixed-name call site and nothing else. The Prometheus recorder, the Sentry recorder, its threads, its tracing layer, and the process sampler meet this. The RocksDB sampler doesn't: it runs with every backend off.
2. **Recording on a kept handle is bounded and lock-free:** atomic operations only, for each enabled backend, with no mutex, SDK call, or I/O. The Sentry policy keeps per-label-set state in a sharded registry, not behind one mutex, and captures only on its flush thread. Histogram values are appended to an atomic bucket, which allocates only when a block of values fills. A record without a kept handle also registers the key with each backend: a registry lookup per backend, and an allocation in the fanout for the combined handle. The first registration of a label set takes the Sentry policy's label-set lock.
3. **Hot call sites keep their handles.** Per-message, per-transaction, and per-proof call sites create their handles once, after the recorder is installed. A handle created earlier stays a no-op for the life of the process, so a test checks the order. Only the connection-state gauge keeps handles today. The per-message counters in `zebra-network/src/protocol/external/codec.rs` and `zebra-network/src/peer/connection.rs` still record with fixed names, and the order test doesn't exist. Both are open.
4. **Labels don't allocate.** Label values on hot paths are static strings, and nothing is formatted before a cheap check decides that a value changed. The per-message `command` labels are static strings, and the connection-state gauge no longer formats. The tip block delay formats its height and network upgrade once per block.
5. **`event.` keys never enter a keyed map.** Every backend splits them off before its registry, so they never create a series or a label set. The value travels as a label in the key, and splitting copies the key's labels on every record. Event attributes are therefore allowed only on histograms recorded at most about once per block.
6. **Background work stays off the async runtime:** flushes, scrapes, `/proc` sampling, and span export run on their own threads. The Sentry flush and transport check threads, the Prometheus exporter's HTTP thread, and the process sampler meet this. The RocksDB sampler runs on Tokio, which is open.
7. **Each backend sends spans only when asked.** The Sentry tracing layer has a span filter that captures nothing. OpenTelemetry should filter spans before building their attributes. That filter is open.
8. **Cost doesn't grow with each backend.** The fanout stays for now: kept handles already meet the per-backend threshold where they are used. The next step is keeping handles at the hot per-message call sites, with the order test from requirement 3. Sentry and the OTLP adapter switch to one shared registry read at flush time only if hot call sites can't keep handles, or if kept-handle cost still exceeds the thresholds on a quiet host. Zakura's own Prometheus recorder is precedent for replacing the exporter behind the facade.

`describe()` runs once at startup, and the catalog, parity, and volume tests are test-only, so neither costs anything at runtime.

**Measurement.** The criterion benchmarks in `zebrad/benches/telemetry.rs` measure counter, gauge, and histogram records with a fixed name, a dynamic label, and a kept handle, plus a histogram with an event attribute. Each runs with no recorder, Prometheus, Prometheus behind the event-attribute layer, Prometheus and the Sentry policy in a fanout, and a fanout of two Prometheus recorders. Span create, enter, and exit run with no subscriber, the registry alone, and the Sentry layer with and without its span filter. Run them with:

```sh
cargo bench -p zebrad --features "sentry prometheus" --bench telemetry
```

The benchmarks run on one thread only. These are criterion means from a heavily loaded shared host, so each value can be off by 20 to 30 percent:

| Record | Cost |
| --- | --- |
| Any record, everything off | 0.2 to 2.3 ns |
| Counter, kept handle, Prometheus | about 7.5 ns |
| Counter, kept handle, Prometheus and Sentry | about 16 ns |
| Histogram, kept handle, Prometheus | about 22 ns |
| Counter, fixed name, Prometheus | about 45 ns |
| Counter, fixed name, fanout of two Prometheus recorders | 111 to 161 ns |
| Counter, fixed name, Prometheus and Sentry | 114 to 140 ns |
| Histogram with an event attribute, Prometheus and Sentry | 420 to 490 ns |

A fanout of two Prometheus recorders costs about as much as Prometheus and Sentry, so the added cost of a fixed-name record comes from the fanout's registration path, not from the Sentry policy.

Not measured yet: records on 16 threads, sync throughput, peak memory, and histogram p50 with spans on and off. The end-to-end baseline is open:

- **End to end:** a checkpoint sync over a fixed height range from a cached state, and full verification near the tip, offline with fixed peers, three runs each. Configurations: compiled out, compiled in and off, each backend alone, and all on. Compare blocks per second, CPU time, peak memory, and p50 and p99 of the block-commit and batch-verification histograms.
- **Profiles:** CPU share of telemetry symbols, allocations per block and per peer message, and lock contention.

Acceptance thresholds, with the status from the measurements above:

| Measure | Threshold | Status |
| --- | --- | --- |
| Fixed-name record, everything off | ≤ 5 ns, no allocation | Met: 0.2 to 2.3 ns. |
| Kept handle, per enabled backend | ≤ 15 ns, no allocation | Met for counters: about 8 ns per backend. A kept histogram costs about 22 ns with Prometheus. |
| Each added backend, per record | ≤ 30 ns more, no extra allocation | Not met for fixed-name records: 70 to 95 ns more with Sentry. Requirement 8 decides the response. |
| 16 threads against 1 | No worse than 2× per record | Not measured. |
| Sync throughput, compiled in and off | Within 0.5% of compiled out | Not measured. |
| Sync throughput, each backend on | Within 1% | Not measured. |
| Sync throughput, all backends on | Within 2% | Not measured. |
| Peak memory, all backends on | ≤ 20 MB more | Not measured. |
| Histogram p50 with Sentry spans on and off | Within 1% | Not measured. The span filter removes the Sentry span work this compares. |

### Expected Consequences

- Adding a signal is one change in the recording crate: description, `LABELS` entry if it has labels, and call. No backend needs a separate change, and the parity test fails if one is left out. The catalog diff shows reviewers the new signal.
- Operators can choose Prometheus, Sentry, OpenTelemetry, or any combination, and get the same signals. The book documents the three backends side by side, with one signal reference generated from the catalog, rather than describing Sentry as production monitoring and Prometheus as the metrics page.
- Sentry users receive more metrics than before: every recorded metric instead of a hand-picked list. The per-flush budget, pacing, backpressure, and label-set cap bound what one flush costs in transport volume, and `zebrad.sentry.metrics.dropped.total` shows what they leave out. Sentry's metric quotas aren't documented, so the bound is on transport volume, not on price.
- Dashboards and rules in the repository can no longer reference metrics that don't exist.
- Contributors learn one rule: record through the facade, describe in the crate's `metrics` module, and follow the label rules. Backend code is in `zebrad`.
- Labels and the cardinality class are enforced by tests and review, not by types. If that proves weak, Option 3 can be adopted later without changing names or backends.
- The checked-in catalog JSON adds a file to update with every metric change. The test prints the regeneration command.
- The `sentry` and `prometheus` features both depend on `metrics-util`, because the Sentry policy, the fanout, and the event-attribute layer use it.
- Every sync from `main` to `next` has to keep `next`'s own metrics cataloged and its queries matching the catalog.

### Gated future work

- **Sentry through OTLP.** If Sentry starts accepting OTLP metrics, the OTLP adapter from phase 6 could replace the Sentry metrics adapter, keeping the Sentry policy's shaping.
- **Configuration parity.** Sentry is turned on only by `SENTRY_*` environment variables, while Prometheus and OpenTelemetry also have `zebrad.toml` settings. Add a Sentry configuration section if operators ask to manage all three the same way.
- **Typed definitions.** Revisit Option 3 if the label or policy checks repeatedly miss errors in review.
- **One shared registry.** Replace the fanout if requirement 8's conditions are met.

## Migration plan

Each phase is one or more public PRs that stand alone. Phases 0 to 3 are implemented, starting with #12434. Their open items are work that phase didn't complete.

| Phase | Change | Result | Status |
| --- | --- | --- | --- |
| 0 | Single-threaded telemetry benchmarks; a span filter on the Sentry tracing layer; no Sentry threads for a disabled client; no label formatting for the connection-state gauge. | Today's costs are measured, and the largest avoidable ones are gone before anything new is added. | Implemented. Open: benchmarks on 16 threads, a benchmark comparison in CI, the end-to-end baseline and profiles, and the OpenTelemetry span filter. |
| 1 | Process metrics from the `metrics-process` crate, which emits the Prometheus process-collector names, sampled on a dedicated thread when a recorder is installed, replacing `process.memory.rss_bytes` before it is released. | Sampled signals reach every backend, independent of Sentry. | Implemented. Open: the RocksDB sampler moves off Tokio and starts only when a backend is enabled. |
| 2 | `describe()` and `LABELS` in each crate, described right after the recorder is installed; generated catalog JSON; call-site and dashboard and rule checks; buckets for the RPC and handshake latency histograms; fixes for the other broken queries and the error-rate alert; unit tests on `docker/observability/` changes. | The catalog exists, and repository dashboards and rules match the code. | Implemented. The generated book reference moves to phase 5. |
| 3 | Backend-neutral node context; Sentry policy with a lock-free record path, sending every recorded metric with per-flush caps, budget, pacing, backpressure, a label-set cap, and drop accounting; parity and volume tests; `event.` label-stripping layer; tip block delay as one histogram; delta sending for absolute counters; build info for every backend; label fixes for `missing_service` and the peer cache path. | Every backend receives every signal, and CI proves it. | Implemented. Open: the backend-neutral exclude setting; kept handles at the per-message call sites with the order test; benchmarks against the thresholds on a quiet host, which settle requirement 8. |
| 4 | Counters for log-only signals (stall, start); one backend-neutral list of operator-relevant log targets; shipped and documented alerts move to counters. Log hygiene: the warning that the chain tip metrics channel closed fires on every node without a health endpoint and reaches Sentry as a log, and the warning that Sentry session rotation couldn't start is logged before tracing is installed, so no one sees it. | Alerts no longer depend on log wording or on one backend. | Not started. |
| 5 | Catalog JSON published with releases; one book page for all three backends, with a signal reference generated from the catalog; z3 copies the fixed dashboards and rules when it bumps its Zebra pin, as it does today. | Operators choose a backend from one signal reference. | Not started. |
| 6 | Native OTLP metrics adapter in the fanout. | OpenTelemetry users get metrics without running a Collector against `/metrics`. | Not started. |

## More Information

### Prior art

Evidence is ranked from strongest to weakest: source code, then specification text, then a recorded rationale, then consensus across projects, then vendor documentation. Each row is labelled "code" (read in the source) or "docs" (documentation only). Sources were read on 2026-11-12.

| Question | Finding | Evidence |
| --- | --- | --- |
| Can Sentry ingest OTLP metrics? | No. Relay's OTLP router serves only `/v1/traces` and `/v1/logs`, and Sentry's OTLP page says metrics aren't supported. | Code: [getsentry/relay `otlp.rs` L18-L29](https://github.com/getsentry/relay/blob/bef9ef855ef5d0f7c04da19a498f49a020600b14/relay-server/src/endpoints/integrations/otlp.rs#L18-L29). Docs: [Sentry OTLP](https://docs.sentry.io/concepts/otlp/). |
| Does the Sentry SDK aggregate metrics? | No. Each `capture()` is queued; the batcher sends 100 items or every 5 seconds; the transport holds 30 envelopes by default. | Code: [sentry-rust `batcher.rs` L15-L19](https://github.com/getsentry/sentry-rust/blob/925803215c3fe7f975e1b49211311f8902dc448d/sentry-core/src/client/batcher.rs#L15-L19), [`transports/mod.rs` L61](https://github.com/getsentry/sentry-rust/blob/925803215c3fe7f975e1b49211311f8902dc448d/sentry/src/transports/mod.rs#L61). Sentry's metric quotas and cardinality limits are undocumented. |
| Is OpenTelemetry's Rust Prometheus exporter usable? | Yes, but Beta. It was un-deprecated in 0.32; the metrics SDK and OTLP exporter are Stable. | Code: [opentelemetry-rust CHANGELOG L21](https://github.com/open-telemetry/opentelemetry-rust/blob/1e29b1fe1d03bde8e48eb564a3dee8ec9efed6a7/opentelemetry-prometheus/CHANGELOG.md#L21), [README L45-L48](https://github.com/open-telemetry/opentelemetry-rust/blob/1e29b1fe1d03bde8e48eb564a3dee8ec9efed6a7/README.md#L45-L48). |
| Is there a bridge from the `metrics` facade to OpenTelemetry? | Two maintained third-party crates (`metrics-exporter-opentelemetry`, `metrics-opentelemetry`), each with a single owner. None in the metrics-rs organization. | Code and crates.io. |
| Does a standard crate emit process metrics with Prometheus names? | Yes. `metrics-process` records `process_resident_memory_bytes`, `process_open_fds`, `process_cpu_seconds_total`, `process_start_time_seconds`, and others through the facade, with descriptions. | Code: `metrics-process` 2.4.3, `process/src/lib.rs`. |
| How does OpenTelemetry name the same signals? | `process.memory.usage`, `process.cpu.time`, `process.unix.file_descriptor.count`. Prometheus translation adds unit and `_total` suffixes. | Docs: OpenTelemetry process semantic conventions (mostly release candidate); Prometheus OTLP guide. |
| Does `metrics-exporter-prometheus` rename metrics when a unit is described? | Not by default. The unit suffix is opt-in. Histograms are summaries unless buckets are set. | Code: `metrics-exporter-prometheus` 0.16.2, `recorder.rs` and `exporter/builder.rs`, as locked by Zebra. |
| How does Reth define metrics? | `#[derive(Metrics)]` structs (the `metrics-derive` crate) over the `metrics` facade; the doc comment becomes the description. Prometheus is the only metrics backend; OTLP carries traces and logs. CI checks only the dashboards' JSON shape. | Code: [reth `tree/metrics.rs` L120-L135](https://github.com/paradigmxyz/reth/blob/58f58ebfb9dd48a2bad94eb9cd85e9f69d0e9891/crates/engine/tree/src/tree/metrics.rs#L120-L135). |
| How does Lighthouse? | `LazyLock` statics with a required help string, Prometheus only, OpenTelemetry for traces. Its published dashboards query `beacon_processor_workers_active_total`, which the code doesn't emit. | Code: [lighthouse `common/metrics`](https://github.com/sigp/lighthouse/blob/57c2abc13078bf949e4d19df6178a644b064c32c/common/metrics/src/lib.rs#L24-L76), sigp/lighthouse-metrics `4eacddef` `BeaconProcessorV2.json`. |
| How does Polkadot? | Name and help at registration, Prometheus only. Alert rules have promtool tests, but no workflow runs them. Its integration tests import metric-name constants. | Code: [polkadot-sdk `service/src/metrics.rs`](https://github.com/paritytech/polkadot-sdk/blob/8d7050787a8552725d826c38cf4beae9ef2467ab/substrate/client/service/src/metrics.rs#L54-L88). |
| How does Agave? | `datapoint_*!` macros to InfluxDB, with typed fields but no description or unit, and no schema check against its dashboard. | Code: [agave `metrics/src/datapoint.rs`](https://github.com/anza-xyz/agave/blob/0263cd36832c3ce77b10ce05e405d1edbcc9c93f/metrics/src/datapoint.rs#L45-L191). |
| How does Zakura (a Zebra fork)? | Kept the `metrics` facade and replaced the Prometheus exporter with its own recorder, keeping names. Removed OpenTelemetry tracing. | Code: [zakura `recorder.rs`](https://github.com/zakura-core/zakura/blob/8cb1351e6c44faf8d51c909fe2be7196f651b8cf/crates/zakurad/src/components/metrics/prometheus/recorder.rs#L1-L40). |
| How does Bitcoin Core? | No metrics exporter; USDT tracepoints for eBPF tools. A Prometheus request was closed as unlikely to happen. | Docs: [doc/tracing.md](https://github.com/bitcoin/bitcoin/blob/master/doc/tracing.md), [bitcoin#10887](https://github.com/bitcoin/bitcoin/issues/10887). |
| What checks dashboards and rules against exported names? | `promtool check rules` and `promtool test rules` check syntax and behavior on given series, not existence. `pint`'s `promql/series` check queries a live Prometheus. kubernetes-mixin regenerates and diffs its output and keeps a checked-in metric list. | Code: [prometheus `cmd/promtool`](https://github.com/prometheus/prometheus/blob/main/cmd/promtool/main.go). Docs: [cloudflare/pint](https://github.com/cloudflare/pint), [kubernetes-mixin CI](https://github.com/kubernetes-monitoring/kubernetes-mixin/blob/main/.github/workflows/ci.yaml). |

What the evidence implies:

- **Copied:** the `metrics` facade as the single recording API with per-backend recorders (Reth, Zakura), and descriptions as part of every definition (Reth, Lighthouse, Polkadot).
- **Copied:** Prometheus process-collector names, through `metrics-process`, because Zebra's dashboards and operators' tooling already use them.
- **Adapted:** kubernetes-mixin's checked-in metric list, generated from `describe_*!` rather than maintained by hand, and used to check hand-written dashboards rather than to diff generated ones.
- **Rejected:** OpenTelemetry metrics as the source, because Sentry can't receive them and the Prometheus path would rename series.
- **Rejected:** a typed derive (Reth's model) for now. It doesn't carry units or backend policy either, and no surveyed node checks its dashboards against it. Lighthouse shows that defining metrics in code doesn't stop dashboards drifting; only a check does.
- **New:** none of the surveyed nodes checks dashboards or alert rules against the code's metric names in CI. Zebra's check has no direct precedent; the closest is the regenerate-and-diff pattern in monitoring mixins.

Not verified: Sentry's metric quotas, cardinality limits, and retention, which decide whether "send everything" could ever be affordable. Sentry's quota documentation or support would settle it.

### Related

- #11629 observability for release confidence, #11631 metrics collection, #11637 fleet metrics collection, #11641 process and runtime metrics.
- #12434 Sentry span filter, the first implementation PR.
- [Bitcoin Core USDT tracing](https://github.com/bitcoin/bitcoin/blob/master/doc/tracing.md).

## Revision History

- 2026-11-12: Proposed.
- 2026-11-13: Recorded the implementation of phases 0 to 3 as the decision: literal call-site names, the Sentry policy's shaping and volume limits, drop accounting, label rules, measured costs, and the requirement 8 decision. Marked unfinished work as open.

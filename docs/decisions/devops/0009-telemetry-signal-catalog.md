---
status: proposed
date: 2026-11-12
story: Define each telemetry signal once and serve Prometheus, Sentry, and OpenTelemetry from it (#11629, #11631, #11641)
---

# Define telemetry signals once, with per-backend export policies

## Context and Problem Statement

Zebra sends telemetry to three backends, and each one is maintained separately:

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
| Sync stalls and node starts exist only as log lines. ZF's Sentry monitors count log text. | Prometheus can't see these conditions, and rewording a log message silently breaks a monitor. |

The question is where a signal is defined, how each backend derives from that definition, and how dashboards and alert rules are kept consistent with it.

## Priorities & Constraints

- **ZF and operators use different backends.** For ZF's own fleets, Sentry is the single source of truth and the only thing that alerts. Public operators use Prometheus and Grafana, and possibly OpenTelemetry. The z3 stack, for example, turns on the metrics endpoint by default, scrapes it with Prometheus, and documents sending traces over OTLP to Jaeger. Zebra must serve both, and neither can be a second-class copy of the other.
- **Existing Prometheus names are a public contract.** Operators' dashboards and alert rules depend on them. Renames need a deprecation period.
- **Backends differ in volume and cardinality.** Sentry stores every value it receives. The SDK queues each `capture()` call, batches 100 items or 5 seconds into an envelope, and keeps at most 30 envelopes in its transport queue, which overflowed during an initial sync. Prometheus keeps one series per label set, so a label like the block height creates unbounded series. Sentry attributes can carry it.
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
- Good, because each backend's policy is a `Recorder` or `metrics-util` layer in one `zebrad` module, composed by the existing fanout. Lower crates stay unaware of backends.
- Good, because comparable Rust nodes use the same facade boundary: Reth records through it with a derive macro that describes each metric, and Zakura replaced its Prometheus exporter behind it without changing names.
- Bad, because `describe_*!` carries no labels, cardinality class, or backend policy. These have to live in the policy module and in conventions, enforced by tests rather than by types.
- Bad, because a call site can still record a name nobody described. A test has to catch it.

#### Option 2: OpenTelemetry metrics as the source

- Good, because OTLP is the cross-vendor standard and OpenTelemetry semantic conventions give ready names for process and runtime metrics.
- Bad, because Sentry doesn't accept OTLP metrics. Sentry's OTLP endpoint and Relay's router take only `/v1/traces` and `/v1/logs`. Zebra would still need a custom Sentry exporter, so one of the three paths stays custom.
- Bad, because the Prometheus path gets weaker. The Rust pull exporter, `opentelemetry-prometheus`, is rated Beta while the metrics SDK and OTLP exporter are Stable, and the project points new users to OTLP push instead, which needs a Collector or Prometheus's OTLP receiver. Either route applies OpenTelemetry's name translation (unit and `_total` suffixes), which renames existing series unless every name is pinned with views or translation settings.
- Bad, because it rewrites every call site in five crates, for no gain to the backend ZF alerts on.

#### Option 3: A Zebra telemetry registry or macro crate

- Good, because a definition could carry everything: kind, unit, labels and their cardinality class, and backend policy, checked at compile time.
- Bad, because it is a new published crate at the bottom of the dependency graph, and a second API beside the facade that every contributor has to learn. The existing `metrics-derive` crate (Reth's) carries descriptions but not units, labels, or policy, so it wouldn't cover the gap alone.
- Bad, because the gain over Option 1 is the compile-time check of labels and policy. The drift observed today is between code and dashboards, which a type system can't see, and tests on a catalog cover it.
- Neutral: Option 1 doesn't rule this out. If the policy tables grow hard to keep consistent, the catalog can move behind a macro without changing names or backends.

#### Option 4: `tracing` events as the source

- Good, because logs and metrics would share one call. `tracing-opentelemetry`'s `MetricsLayer` derives metrics from event fields with `counter.` or `histogram.` prefixes.
- Bad, because metric identity becomes a naming convention on log fields. Kind and unit are encoded in field names, there's no description, and filtering a log level or target would drop metrics.
- Bad, because the hot paths that record metrics today would pay for event dispatch through every subscriber layer.
- Bad, because it only bridges to OpenTelemetry metrics, so it inherits Option 2's Sentry and Prometheus problems.

## Decision Outcome

Chosen option: **Option 1**. The `metrics` facade stays the single place where values are recorded. Each crate describes the metrics it records in one module, the union of those descriptions is the **signal catalog**, and every backend, dashboard, and alert rule is derived from or checked against that catalog.

The design has six parts.

### 1. Definition: one `metrics` module per crate

Each crate that records metrics gets a `metrics` module with:

- a constant for each metric name, which call sites use instead of string literals;
- a `pub fn describe()` that calls `metrics::describe_counter!`, `describe_gauge!`, or `describe_histogram!` for each name, with its `metrics::Unit` and a one-line description;
- a `pub const LABELS: &[(&str, &[&str])]` table listing each labelled metric's label keys, with a doc comment naming each label's allowed values, for example "`reason`: one of `queue_full`, `no_serving_peer`".

`zebrad` calls every crate's `describe()` immediately after it installs the global recorder, and before any component starts. Descriptions issued while no recorder is installed go to the `metrics` crate's no-op recorder and are lost. If no backend is enabled, there is nothing to describe to.

The catalog is the set of described metrics. A metric's **kind** comes from which `describe_*!` macro was used, and its **unit** and **description** from the macro's arguments. Its **labels** come from the `LABELS` table. Each label key has one of two **cardinality classes**:

- **Labels** have a small, fixed set of values: an enum, a result, a network kind, a protocol command. Every backend receives them. A value taken from peer input is mapped to a fixed set first. The existing `remote_services` label, which formats a peer's service bits, is the one case to fix.
- **Event attributes** may take a new value on every record, such as a block height. Their keys start with the reserved prefix `event.`, for example `event.chain.height`, so that the call site itself says which class a key is in, and no list has to be kept in sync across crates. Event attributes are allowed only on histograms.

Backends treat an `event.` key as unsafe by default. The Prometheus policy drops it, so a new event attribute added in any crate can never become a Prometheus series. The Sentry policy removes the prefix and keeps it as an attribute (`chain.height`, the name Sentry dashboards already use), and drops it from counters and gauges, whose per-label-set state is kept for the life of the process.

New names follow the existing dot-separated prefixes and end in their unit (`_seconds`, `_bytes`). Process metrics use the Prometheus process collector's names (`process_resident_memory_bytes` and so on), because operators' dashboards, including Zebra's own, already query them.

### 2. Collection: sampling never depends on a backend

Values that are sampled rather than recorded when something happens, such as process memory, open file descriptors, and database size, are sampled by one telemetry task in `zebrad`, on a fixed interval, whether or not any backend is enabled. No backend's code computes a value. This prevents the memory-gauge drift: a backend's flush loop reads the latest value like any other gauge.

### 3. Export: one policy per backend, in one `zebrad` module

All backend code lives in one `zebrad` module (today `components/metrics.rs` and `sentry/`). Each backend is a `Recorder`, wrapped in `metrics-util` layers, and combined in the existing fanout:

| | Prometheus | Sentry | OpenTelemetry |
| --- | --- | --- | --- |
| Which metrics | All catalog metrics. | An opt-in list of catalog names, kept in the Sentry policy and checked against the catalog. | None directly. A Collector scrapes the Prometheus endpoint with its `prometheus` receiver. |
| Counters | Cumulative, as today. | The increase over each 60-second interval, per label set. Counters set with `absolute()`, such as `process_cpu_seconds_total`, send the difference from the previous flush, not the running total. | Through the scrape. |
| Gauges | Latest value at scrape. | Latest value every 60 seconds. | Through the scrape. |
| Histograms | Real Prometheus histograms for the metrics listed in the policy's bucket table (`set_buckets_for_metric`), so they aggregate across nodes. The rest stay summaries. | Distributions, only for names on the policy's distribution list. Each entry says whether it is sent only near the tip (block phase timings) or always (the tip block delay, which skips backlog blocks itself). | Through the scrape. |
| Event attributes | Dropped by a label-stripping layer, so a height never becomes a series. | Kept on distributions, without the `event.` prefix. Dropped from counters and gauges. | Dropped (through the scrape). |
| Context | Prometheus target labels. | `before_send_metric` adds network, network upgrade, and uptime, as today. | Collector resource attributes. |
| Names | Dots become underscores, as today. Unit suffixes stay off, so names don't change. | Dotted names, as today. | Prometheus names. |

The tip block delay becomes one call: a histogram with an `event.chain.height` event attribute. Prometheus drops the attribute and Sentry keeps it, so `send_tip_block_delay` and its second code path go away. The Sentry policy keeps today's behavior of deriving `chain.network_upgrade` from the block's own height when a distribution has that attribute, rather than from the tip height, which lags behind a new block.

The Sentry list stays opt-in rather than sending every catalog metric. Every value is stored and billed, the transport queue holds 30 envelopes, and Sentry's per-attribute and per-project metric limits aren't documented. Each addition is a deliberate choice of what ZF alerts on, and the catalog check guarantees that each name on the list exists.

OpenTelemetry stays traces-only. Operators who want metrics in an OpenTelemetry pipeline scrape `/metrics` with a Collector; the book's metrics page documents the receiver configuration. Native OTLP metrics push is deferred until a trigger listed under Gated future work is met.

### 4. Drift checks: the catalog is a generated, checked file

A `zebrad` test installs a small recorder that keeps every `describe_*` call, calls every crate's `describe()`, merges in each crate's `LABELS`, and writes the catalog as JSON (name, kind, unit, description, label keys, and Prometheus exposition names). `metrics-util`'s `DebuggingRecorder` doesn't fit: its snapshots leave out metrics that are described but never recorded. The JSON is checked in, and the test fails if it is stale, so every change to a signal shows up in review as a catalog diff. The book's metrics reference is generated from the same data.

Four checks run against the catalog in CI:

1. **Call sites.** A test scans workspace sources for `counter!`, `gauge!`, and `histogram!` calls and fails if a literal name, or a label key, is not in the catalog. Every name constant in a `metrics` module must be described, and every `LABELS` entry must name a described metric.
2. **Dashboards and alert rules.** A test extracts metric names from the PromQL in `docker/observability/grafana/dashboards/*.json`, `docker/observability/prometheus/rules/*.yml`, and the example queries in `docker/observability/README.md`, and maps them back to catalog entries using the Prometheus naming rules. A `_bucket` query matches only a histogram in the bucket table, and a `quantile` label only one that isn't. The test fails on any unknown name. A short allowlist covers series Zebra doesn't produce, such as `up` and the Jaeger span metrics.
3. **Backend policies.** Unit tests check that every name in the Sentry lists and the bucket table is in the catalog with the right kind, and that `event.` keys appear only on histograms.
4. **External consumers.** z3 already copies the dashboards and rules from the Zebra release it pins, so fixing them in Zebra reaches z3 at its next version bump, and z3 needs no check of its own while it keeps them unmodified. Copies that are changed, and ZF's private Sentry dashboard and monitor definitions, check their queries against the catalog JSON of the Zebra release they target, using the same mapping. Zebra publishes the file with each release; those repositories run the check.

Checking was chosen over generating dashboards and rules from the catalog. Generation (for example jsonnet mixins) pays off only with a regenerate-and-diff gate, and it would replace hand-edited Grafana JSON that operators copy. The checks catch the drift observed today at a fraction of the cost.

### 5. Logs as signals: alerts target metrics, not log text

When an alert or dashboard needs to know that something happened, the call site that logs it also increments a counter in the catalog: for example a sync stall, or a node start (the existing build info series plus `process_start_time_seconds`). Non-finalized fork evictions already follow this pattern with `state.memory.fork.evicted.count`. The log line stays for context. Alerts in every backend, including ZF's Sentry monitors, move from matching log text to these counters, so rewording a message can't silently break an alert.

### 6. Renames and deprecation

Existing exported names don't change as a side effect of this work. When a name must change:

1. The Prometheus policy records the metric under both names for at least one minor release, using a rename table in the policy module.
2. The release adds a `Deprecated` change fragment naming the old and new series.
3. The following release removes the old name with a `Removed` fragment.

Turning an existing summary into a bucketed histogram is also a breaking change, even though the name stays: its `quantile` series disappear, and the exporter can't serve both forms under one name. It follows the same steps, with the `Deprecated` fragment one release before the buckets are added. Exception: a metric whose only queries in the repository use `_bucket`, which today return nothing, may be bucketed directly with a `Changed` fragment. That covers `rpc.request.duration_seconds` and `zcash.net.peer.handshake.duration_seconds`.

Dashboard and rule queries for series that never existed, like those in the table above, are fixed directly. No operator received data from them, so there's nothing to deprecate.

### Expected Consequences

- Adding a signal is one change in the recording crate (constant, description, call) plus, when ZF should alert on it, one line in the Sentry policy. The catalog diff shows reviewers both.
- Prometheus users get every signal, including the ones added for Sentry. ZF's Sentry gets an explicit subset with known cost.
- Dashboards and rules in the repository can no longer reference metrics that don't exist, and broken ones found by the first check get fixed.
- Contributors learn one rule: record through the facade, describe in the crate's `metrics` module. Backend code is in one place.
- Labels and the cardinality class are enforced by tests and review, not by types. If that proves weak, Option 3 can be adopted later without changing names or backends.
- The checked-in catalog JSON adds a file to update with every metric change. The test prints the regeneration command.
- `metrics-util` becomes a dependency of the telemetry module rather than of the `prometheus` feature alone, because the Sentry policy and the label-stripping layer use its layers.

### Gated future work

- **OTLP metrics push.** Add an OpenTelemetry metrics exporter as another recorder in the fanout when an operator needs push without a Collector scraping `/metrics`, or when Sentry accepts OTLP metrics and could replace the custom Sentry recorder.
- **Sentry send-all with budgets.** Revisit the opt-in list if Sentry documents metric quotas that make sending every counter and gauge once a minute predictable in cost.
- **Typed definitions.** Revisit Option 3 if the label or policy checks repeatedly miss errors in review.

## Migration plan

Each phase is one or more public PRs that stand alone.

| Phase | Change | Result |
| --- | --- | --- |
| 1 | Telemetry sampling task in `zebrad`; process metrics from the `metrics-process` crate, which emits the Prometheus process-collector names, replacing `process.memory.rss_bytes` before it is released. | Sampled signals reach every backend, independent of Sentry. |
| 2 | `metrics` modules, `describe()`, and `LABELS` in each crate, described right after the recorder is installed; generated catalog JSON; call-site and dashboard/rule checks; buckets for the RPC and handshake latency histograms; fixes for the other broken queries and the error-rate alert. | The catalog exists, and repository dashboards and rules match the code. |
| 3 | Sentry policy checked against the catalog; `event.` label-stripping layer; tip block delay as one histogram with an event attribute; delta sending for absolute counters. | One call site per signal; per-backend differences live only in policy. |
| 4 | Counters for log-only signals (stall, fork eviction, start); ZF monitors move to them. | Alerts no longer depend on log wording. |
| 5 | Catalog JSON published with releases; ZF's Sentry definitions check against it; z3 copies the fixed dashboards and rules when it bumps its Zebra pin, as it does today. | No copy of a dashboard drifts unnoticed. |

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
- [Bitcoin Core USDT tracing](https://github.com/bitcoin/bitcoin/blob/master/doc/tracing.md).

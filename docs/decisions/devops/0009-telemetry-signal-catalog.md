---
status: proposed
date: 2026-11-12
story: Define each telemetry signal once and serve Prometheus, Sentry, and OpenTelemetry from it (#11629, #11631, #11641)
---

# Define telemetry signals once and give every backend the same signals

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
| Sentry receives about ten hand-picked metrics, out of more than 170 names that Prometheus receives. | A Sentry user can't see most of what a Prometheus user can, and the list grows only when someone remembers it. |
| Sync stalls and node starts exist only as log lines, so Sentry alerts on them have to match log text. | Prometheus users can't alert on these conditions, and rewording a log message silently breaks an alert. |

Drift runs in every direction: signals added for Sentry miss Prometheus users, signals in Prometheus miss Sentry users, and dashboards miss both. The question is where a signal is defined, how every backend derives from that one definition, and how dashboards and alert rules are kept consistent with it.

## Priorities & Constraints

- **Backends are peers.** Prometheus, Sentry, and OpenTelemetry are all operator-facing, and an operator may run any combination of them. Prometheus support and Sentry support have both shipped in release builds for years and are documented in the book. The z3 stack turns on the metrics endpoint by default and documents sending traces over OTLP to Jaeger. ZF's own fleets happen to alert from Sentry, but that is one operator's choice, not a property of the backend. An operator must be able to pick a backend knowing they get the same signals as anyone else, so a signal added for one backend and forgotten in the others is a defect.
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
- Bad, because it rewrites every call site in five crates, and Prometheus and Sentry users get nothing they lack today.

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

One rule governs every backend: **every catalog signal reaches every enabled backend.** A backend's policy may change how a signal is shaped for it, such as aggregation intervals, histogram form, or which attributes it can store, but never which signals it receives. A test enforces this (part 5), so it doesn't depend on contributors remembering each backend.

The design has seven parts.

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

Values that are sampled rather than recorded when something happens are sampled by the module that owns them, on a fixed interval, whether or not any backend is enabled. The state already samples its database size this way every 30 seconds. Process-wide values, such as memory, open file descriptors, and CPU time, are sampled by one task in `zebrad`. No backend's code computes a value. This prevents the memory-gauge drift: a backend's flush loop reads the latest value like any other gauge.

### 3. Export: one policy per backend, in one `zebrad` module

All backend code lives in one `zebrad` module (today `components/metrics.rs` and `sentry/`). Each backend is a `Recorder`, wrapped in `metrics-util` layers, and combined in the existing fanout. Each policy decides only how to shape signals for its backend, and emits plain values to a thin adapter that talks to the backend's library, so a test adapter can stand in for it.

| | Prometheus | Sentry | OpenTelemetry |
| --- | --- | --- | --- |
| Which metrics | All catalog metrics. | All catalog metrics. | All catalog metrics, today by a Collector scraping the Prometheus endpoint with its `prometheus` receiver. |
| Counters | Cumulative, as today. | The increase over each 60-second interval, per label set. Counters set with `absolute()`, such as `process_cpu_seconds_total`, send the difference from the previous flush, not the running total. | Through the scrape. |
| Gauges | Latest value at scrape. | Latest value every 60 seconds. | Through the scrape. |
| Histograms | Real Prometheus histograms for the metrics listed in the policy's bucket table (`set_buckets_for_metric`), so they aggregate across nodes. The rest stay summaries. | Distributions. Each histogram sends at most a fixed number of values per label set per 60-second interval, chosen at random from that interval's values, so a histogram recorded for every block during an initial sync can't flood the transport. Low-rate histograms, such as the tip block delay, stay under the cap and are sent in full. | Through the scrape. |
| Event attributes | Dropped by a label-stripping layer, because each value would become a series. | Kept on distributions, without the `event.` prefix. Dropped from counters and gauges. | Dropped (through the scrape). |
| Context | Prometheus target labels. | Network, network upgrade, uptime, and whether the node is near the tip, added to every metric, as today. | Collector resource attributes. |
| Names | Dots become underscores, as today. Unit suffixes stay off, so names don't change. | Dotted names, as today. | Prometheus names. |

The rows differ only where a backend can't represent a signal the same way. Event attributes are the one place where a backend receives less: Prometheus can't store a per-block value as a label. The signal itself still reaches Prometheus, and the catalog documents the difference.

Sentry replaces today's allowlist with every catalog metric, because a list of what one operator alerts on is exactly the drift this decision removes. Volume stays bounded by shape, not by selection. A counter or gauge costs one value per label set per minute, and a histogram at most the cap per label set per minute. A test runs the Sentry policy over the whole catalog and checks that one flush fits within the SDK's transport queue (30 envelopes of up to 100 items), so a new signal can't silently push Sentry users into dropped data. The near-tip gate on block phase timings becomes a `chain.near_tip` attribute instead, so queries choose which values to count, and nothing is discarded at the source.

An operator who wants less volume in one backend can exclude names, with one backend-neutral setting that applies to every enabled backend, and per-backend overrides where a backend's cost model needs it. These are the operator's choice, not a Zebra default.

The tip block delay becomes one call: a histogram with an `event.chain.height` event attribute and a `chain.network_upgrade` label set by the call site, which already knows the block's height and network. Prometheus drops the event attribute and Sentry keeps it, so `send_tip_block_delay` and its second code path go away, and the Sentry policy needs no knowledge of the chain.

Chain context that several backends attach, such as the network, the tip height, and whether the node is near the tip, moves from the Sentry module into a backend-neutral node context. The sync progress task writes it in every build, not only builds with the `sentry` feature, and any policy or log enricher reads it.

OpenTelemetry receives metrics through the scrape today. It gets the same catalog, but needs a Collector to read it. A native OTLP metrics adapter, another recorder in the fanout, is phase 6 of the migration, so OpenTelemetry users get push delivery without extra infrastructure.

### 4. Traces and logs follow the same rule

Spans and log events are recorded once through `tracing`, and each backend's layer reads the same events: the Sentry layer and the OpenTelemetry layer both consume every span today. What each backend receives is decided by one shared classification, not by per-backend lists:

- Every backend that takes logs receives WARN and ERROR events, plus INFO events from a shared list of operator-relevant targets, such as the once-a-minute sync progress log. That list is backend-neutral. Sentry Logs uses it today. If an OpenTelemetry logs exporter is added, it uses the same list.
- Errors and panics go to every backend that represents them: Sentry issues today, and OpenTelemetry error events on spans.
- Release health sessions have no equivalent in Prometheus or OpenTelemetry. Their content is also available as metrics: start time, panics, and uptime.

A condition operators need to alert on is never available only as a log or a Sentry issue. It also has a counter (part 6), so Prometheus users can alert on it too.

### 5. Drift checks: the catalog is a generated, checked file

A `zebrad` test installs a small recorder that keeps every `describe_*` call, calls every crate's `describe()`, merges in each crate's `LABELS`, and writes the catalog as JSON (name, kind, unit, description, label keys, and Prometheus exposition names). `metrics-util`'s `DebuggingRecorder` doesn't fit: its snapshots leave out metrics that are described but never recorded. The JSON is checked in, and the test fails if it is stale, so every change to a signal shows up in review as a catalog diff. The book's metrics reference is generated from the same data.

Five checks run against the catalog in CI:

1. **Backend parity.** A test records one value for every catalog metric through the full fanout, with every backend policy connected to a test adapter, and checks that each backend received every metric: present in the rendered Prometheus output, and emitted by the Sentry policy at the next flush. The same test checks the Sentry volume budget from part 3. A signal that reaches only some backends fails CI, whichever backend it was added for.
2. **Call sites.** A test scans workspace sources for `counter!`, `gauge!`, and `histogram!` calls and fails if a literal name, or a label key, is not in the catalog. Every name constant in a `metrics` module must be described, and every `LABELS` entry must name a described metric.
3. **Dashboards and alert rules.** A test extracts metric names from the PromQL in `docker/observability/grafana/dashboards/*.json`, `docker/observability/prometheus/rules/*.yml`, and the example queries in `docker/observability/README.md`, and maps them back to catalog entries using the Prometheus naming rules. A `_bucket` query matches only a histogram in the bucket table, and a `quantile` label only one that isn't. The test fails on any unknown name. A short allowlist covers series Zebra doesn't produce, such as `up` and the Jaeger span metrics.
4. **Backend policies.** Unit tests check that every name in the bucket table and the rename table is in the catalog with the right kind, and that `event.` keys appear only on histograms. The block phase histograms that Sentry receives today are recorded only on the `next` branch, so the catalog on `main` describes them too, or they are cataloged on `next`.
5. **External consumers.** z3 already copies the dashboards and rules from the Zebra release it pins, so fixing them in Zebra reaches z3 at its next version bump, and z3 needs no check of its own while it keeps them unmodified. Copies that are changed, and any operator's Sentry dashboards and alerts, including ZF's, check their queries against the catalog JSON of the Zebra release they target, using the same mapping. Zebra publishes the file with each release; those repositories run the check.

Checking was chosen over generating dashboards and rules from the catalog. Generation (for example jsonnet mixins) pays off only with a regenerate-and-diff gate, and it would replace hand-edited Grafana JSON that operators copy. The checks catch the drift observed today at a fraction of the cost.

### 6. Logs as signals: alerts target metrics, not log text

When an alert or dashboard needs to know that something happened, the call site that logs it also increments a counter in the catalog: for example a sync stall, or a node start (the existing build info series plus `process_start_time_seconds`). Non-finalized fork evictions already follow this pattern with `state.memory.fork.evicted.count`. The log line stays for context. Zebra's shipped alert rules, and the alerts it documents for any backend, use these counters rather than log text, so rewording a message can't silently break an alert, and a Prometheus user can alert on the same conditions as a Sentry user.

### 7. Renames and deprecation

Existing exported names don't change as a side effect of this work. When a name must change:

1. The Prometheus policy records the metric under both names for at least one minor release, using a rename table in the policy module.
2. The release adds a `Deprecated` change fragment naming the old and new series.
3. The following release removes the old name with a `Removed` fragment.

Turning an existing summary into a bucketed histogram is also a breaking change, even though the name stays: its `quantile` series disappear, and the exporter can't serve both forms under one name. It follows the same steps, with the `Deprecated` fragment one release before the buckets are added. Exception: a metric whose only queries in the repository use `_bucket`, which today return nothing, may be bucketed directly with a `Changed` fragment. That covers `rpc.request.duration_seconds` and `zcash.net.peer.handshake.duration_seconds`.

Dashboard and rule queries for series that never existed, like those in the table above, are fixed directly. No operator received data from them, so there's nothing to deprecate.

### Expected Consequences

- Adding a signal is one change in the recording crate: constant, description, and call. No backend needs a separate change, and the parity test fails if one is left out. The catalog diff shows reviewers the new signal.
- Operators can choose Prometheus, Sentry, OpenTelemetry, or any combination, and get the same signals. The book documents the three backends side by side, with one signal reference generated from the catalog, rather than describing Sentry as production monitoring and Prometheus as the metrics page.
- Sentry users receive more metrics than today: every catalog metric instead of the current hand-picked list. The volume test bounds what one flush costs, and operators can exclude names. Sentry's metric quotas aren't documented, so the bound is on transport volume, not on price.
- Dashboards and rules in the repository can no longer reference metrics that don't exist, and broken ones found by the first check get fixed.
- Contributors learn one rule: record through the facade, describe in the crate's `metrics` module. Backend code is in one place.
- Labels and the cardinality class are enforced by tests and review, not by types. If that proves weak, Option 3 can be adopted later without changing names or backends.
- The checked-in catalog JSON adds a file to update with every metric change. The test prints the regeneration command.
- `metrics-util` becomes a dependency of the telemetry module rather than of the `prometheus` feature alone, because the Sentry policy and the label-stripping layer use its layers.

### Gated future work

- **Sentry through OTLP.** If Sentry starts accepting OTLP metrics, the OTLP adapter from phase 6 could replace the Sentry metrics adapter, keeping the Sentry policy's shaping.
- **Configuration parity.** Sentry is turned on only by `SENTRY_*` environment variables, while Prometheus and OpenTelemetry also have `zebrad.toml` settings. Add a Sentry configuration section if operators ask to manage all three the same way.
- **Typed definitions.** Revisit Option 3 if the label or policy checks repeatedly miss errors in review.

## Migration plan

Each phase is one or more public PRs that stand alone.

| Phase | Change | Result |
| --- | --- | --- |
| 1 | Telemetry sampling task in `zebrad`; process metrics from the `metrics-process` crate, which emits the Prometheus process-collector names, replacing `process.memory.rss_bytes` before it is released. | Sampled signals reach every backend, independent of Sentry. |
| 2 | `metrics` modules, `describe()`, and `LABELS` in each crate, described right after the recorder is installed; generated catalog JSON; call-site and dashboard/rule checks; buckets for the RPC and handshake latency histograms; fixes for the other broken queries and the error-rate alert. | The catalog exists, and repository dashboards and rules match the code. |
| 3 | Backend-neutral node context; Sentry policy with a test adapter, sending every catalog metric with per-interval caps; parity and volume tests; `event.` label-stripping layer; tip block delay as one histogram; delta sending for absolute counters; backend-neutral exclude setting. | Every backend receives every signal, and CI proves it. |
| 4 | Counters for log-only signals (stall, start); one backend-neutral list of operator-relevant log targets; shipped and documented alerts move to counters. | Alerts no longer depend on log wording or on one backend. |
| 5 | Catalog JSON published with releases; one book page for all three backends; z3 copies the fixed dashboards and rules when it bumps its Zebra pin, as it does today. | Operators choose a backend from one signal reference. |
| 6 | Native OTLP metrics adapter in the fanout. | OpenTelemetry users get metrics without running a Collector against `/metrics`. |

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

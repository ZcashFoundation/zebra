//! Checks what the Prometheus exporter, configured as `zebrad` configures it, renders.

use metrics_exporter_prometheus::{PrometheusHandle, PrometheusRecorder};

use super::{
    super::{prometheus_builder, HISTOGRAM_BUCKETS},
    prometheus_name, Catalog, Kind,
};

/// A histogram that isn't in [`HISTOGRAM_BUCKETS`], so the exporter renders it as a summary.
const SUMMARY_HISTOGRAM: &str = "sync.stage.duration_seconds";

/// Returns the exporter's recorder, built with `zebrad`'s configuration, and its render handle.
fn recorder() -> (PrometheusRecorder, PrometheusHandle) {
    let recorder = prometheus_builder()
        .expect("HISTOGRAM_BUCKETS bounds are finite and non-empty")
        .build_recorder();
    let handle = recorder.handle();
    (recorder, handle)
}

/// Returns the rendered lines of the series `series`, with or without labels.
fn series_lines<'a>(rendered: &'a str, series: &str) -> Vec<&'a str> {
    rendered
        .lines()
        .filter(|line| {
            line.strip_prefix(series)
                .is_some_and(|rest| rest.starts_with('{') || rest.starts_with(' '))
        })
        .collect()
}

/// The histograms in [`HISTOGRAM_BUCKETS`] are exported as Prometheus histograms with their
/// buckets, and every other histogram as a summary.
#[test]
fn exporter_renders_bucketed_histograms_and_summaries() {
    let _init_guard = zebra_test::init();

    assert!(
        HISTOGRAM_BUCKETS
            .iter()
            .all(|(name, _)| *name != SUMMARY_HISTOGRAM),
        "{SUMMARY_HISTOGRAM} must not be bucketed for this test",
    );
    assert_eq!(
        Catalog::build()
            .get(SUMMARY_HISTOGRAM)
            .map(|entry| entry.kind),
        Some(Kind::Histogram),
        "{SUMMARY_HISTOGRAM} must be a described histogram",
    );

    let (recorder, handle) = recorder();
    metrics::with_local_recorder(&recorder, || {
        metrics::histogram!("rpc.request.duration_seconds", "method" => "getinfo").record(0.003);
        metrics::histogram!(
            "zcash.net.peer.handshake.duration_seconds",
            "result" => "success"
        )
        .record(0.2);
        metrics::histogram!(SUMMARY_HISTOGRAM, "stage" => "obtain_tips").record(1.5);
    });
    let rendered = handle.render();

    for (name, buckets) in HISTOGRAM_BUCKETS {
        let name = prometheus_name(name);

        assert!(
            rendered
                .lines()
                .any(|line| line == format!("# TYPE {name} histogram")),
            "{name} is rendered as a histogram:\n{rendered}",
        );

        let bucket_lines = series_lines(&rendered, &format!("{name}_bucket"));
        let bounds: Vec<String> = buckets
            .iter()
            .map(f64::to_string)
            .chain(["+Inf".to_owned()])
            .collect();
        for bound in &bounds {
            let le = format!("le=\"{bound}\"");
            assert!(
                bucket_lines.iter().any(|line| line.contains(&le)),
                "{name} has a bucket with {le}:\n{rendered}",
            );
        }
        assert_eq!(
            bucket_lines.len(),
            bounds.len(),
            "{name} has only the configured buckets:\n{rendered}",
        );

        assert_eq!(series_lines(&rendered, &format!("{name}_sum")).len(), 1);
        assert_eq!(series_lines(&rendered, &format!("{name}_count")).len(), 1);
        assert!(
            series_lines(&rendered, &name).is_empty(),
            "{name} has no quantile series:\n{rendered}",
        );
    }

    let summary = prometheus_name(SUMMARY_HISTOGRAM);
    assert!(
        rendered
            .lines()
            .any(|line| line == format!("# TYPE {summary} summary")),
        "{summary} is rendered as a summary:\n{rendered}",
    );
    assert!(
        series_lines(&rendered, &summary)
            .iter()
            .all(|line| line.contains("quantile=\"")),
        "{summary} series are quantiles:\n{rendered}",
    );
    assert!(!series_lines(&rendered, &summary).is_empty());
    assert!(series_lines(&rendered, &format!("{summary}_bucket")).is_empty());
    assert_eq!(series_lines(&rendered, &format!("{summary}_sum")).len(), 1);
    assert_eq!(
        series_lines(&rendered, &format!("{summary}_count")).len(),
        1
    );
}

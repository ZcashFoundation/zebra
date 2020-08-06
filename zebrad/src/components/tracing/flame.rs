//! An HTTP endpoint for dynamically setting tracing filters.

use crate::config::TracingSection;
use abscissa_core::trace::Tracing;
use color_eyre::eyre::Report;
use std::{
    fs::File,
    io::{BufReader, BufWriter},
    path::PathBuf,
    sync::Arc,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Clone)]
pub struct FlameGrapher {
    guard: Arc<tracing_flame::FlushGuard<BufWriter<File>>>,
    path: PathBuf,
}

impl FlameGrapher {
    fn make_flamegraph(&self) -> Result<(), Report> {
        self.guard.flush()?;
        let out_path = self.path.with_extension("svg");
        let inf = File::open(&self.path)?;
        let reader = BufReader::new(inf);

        let out = File::create(out_path)?;
        let writer = BufWriter::new(out);

        let mut opts = inferno::flamegraph::Options::default();
        info!("writing flamegraph to disk...");
        inferno::flamegraph::from_reader(&mut opts, reader, writer)?;

        Ok(())
    }
}

impl Drop for FlameGrapher {
    fn drop(&mut self) {
        match self.make_flamegraph() {
            Ok(()) => {}
            Err(report) => {
                warn!(
                    "Error while constructing flamegraph during shutdown: {:?}",
                    report
                );
            }
        }
    }
}

pub fn init(config: &TracingSection) -> (Tracing, Option<FlameGrapher>) {
    // Construct a tracing subscriber with the supplied filter and enable reloading.
    let builder = tracing_subscriber::FmtSubscriber::builder()
        .with_env_filter(config.env_filter())
        .with_filter_reloading();
    let filter_handle = builder.reload_handle();
    let subscriber = builder.finish().with(tracing_error::ErrorLayer::default());

    let guard = if let Some(flamegraph_path) = config.flamegraph.as_deref() {
        let flamegraph_path = flamegraph_path.with_extension("folded");
        let (flame_layer, guard) = tracing_flame::FlameLayer::with_file(&flamegraph_path).unwrap();
        let flame_layer = flame_layer
            .with_empty_samples(false)
            .with_threads_collapsed(true);
        subscriber.with(flame_layer).init();
        Some(FlameGrapher {
            guard: Arc::new(guard),
            path: flamegraph_path,
        })
    } else {
        subscriber.init();
        None
    };

    //(filter_handle.into(), guard)
    unimplemented!()
}

pub fn init_backup(config: &TracingSection) {
    tracing_subscriber::Registry::default()
        .with(config.env_filter())
        .with(tracing_error::ErrorLayer::default())
        .init();
}

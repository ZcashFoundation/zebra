//! A tracing flamegraph component.

use color_eyre::eyre::Report;
use std::{
    fs::File,
    io::{BufReader, BufWriter},
    path::Path,
    path::PathBuf,
};
use tracing::Subscriber;
use tracing_subscriber::{registry::LookupSpan, Layer};

pub struct Grapher {
    guard: tracing_flame::FlushGuard<BufWriter<File>>,
    path: PathBuf,
}

pub fn layer<S>(path_root: &Path) -> (impl Layer<S>, Grapher)
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    let path = path_root.with_extension("folded");
    let (layer, guard) = tracing_flame::FlameLayer::with_file(&path).expect("path should be valid");
    let layer = layer.with_empty_samples(false).with_threads_collapsed(true);
    let flamegrapher = Grapher { guard, path };
    (layer, flamegrapher)
}

impl Grapher {
    pub fn write_flamegraph(&self) -> Result<(), Report> {
        self.guard.flush()?;
        let out_path = self.path.with_extension("svg");
        let inf = File::open(&self.path)?;
        let reader = BufReader::new(inf);

        let out = File::create(out_path)?;
        let writer = BufWriter::new(out);

        let mut opts = inferno::flamegraph::Options::default();
        debug!("writing flamegraph to disk...");
        inferno::flamegraph::from_reader(&mut opts, reader, writer)?;

        Ok(())
    }
}

//! Tests for recording the first block template served on each best tip publication.

use std::{
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use metrics::{
    Counter, Gauge, Histogram, HistogramFn, Key, KeyName, Metadata, Recorder, SharedString, Unit,
};

use zebra_chain::{block, chain_tip::ChainTip, transaction, BoxError};

use crate::methods::FirstTemplateTip;

/// The name of the histogram these tests check.
const FIRST_TEMPLATE: &str = "rpc.getblocktemplate.first_template.duration_seconds";

/// The samples recorded to one histogram.
#[derive(Default)]
struct Samples(Mutex<Vec<f64>>);

impl HistogramFn for Samples {
    fn record(&self, value: f64) {
        self.0
            .lock()
            .expect("recording never panics while holding the lock")
            .push(value);
    }
}

/// Records the samples of the first-template histogram and ignores every other metric.
#[derive(Default)]
struct FirstTemplateRecorder(Arc<Samples>);

impl FirstTemplateRecorder {
    fn samples(&self) -> Vec<f64> {
        self.0
             .0
            .lock()
            .expect("recording never panics while holding the lock")
            .clone()
    }
}

impl Recorder for FirstTemplateRecorder {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn register_counter(&self, _: &Key, _: &Metadata<'_>) -> Counter {
        Counter::noop()
    }

    fn register_gauge(&self, _: &Key, _: &Metadata<'_>) -> Gauge {
        Gauge::noop()
    }

    fn register_histogram(&self, key: &Key, _: &Metadata<'_>) -> Histogram {
        if key.name() == FIRST_TEMPLATE {
            Histogram::from_arc(self.0.clone())
        } else {
            Histogram::noop()
        }
    }
}

/// A best tip published at `changed_at`, whose publication reads can be paused.
struct PublishedTip {
    hash: block::Hash,
    changed_at: Instant,

    /// When set, a publication read signals the sender once it starts, then waits for the
    /// receiver before it returns.
    pause: Option<(mpsc::SyncSender<()>, Mutex<mpsc::Receiver<()>>)>,
}

impl ChainTip for PublishedTip {
    fn best_tip_height(&self) -> Option<block::Height> {
        None
    }

    fn best_tip_hash(&self) -> Option<block::Hash> {
        Some(self.hash)
    }

    fn best_tip_height_and_hash(&self) -> Option<(block::Height, block::Hash)> {
        None
    }

    fn best_tip_block_time(&self) -> Option<DateTime<Utc>> {
        None
    }

    fn best_tip_changed_at(&self, expected_hash: block::Hash) -> Option<Instant> {
        if let Some((started, release)) = &self.pause {
            started
                .send(())
                .expect("the test waits for the read to start");
            release
                .lock()
                .expect("only this read locks the receiver")
                .recv()
                .expect("the test releases the read");
        }

        (expected_hash == self.hash).then_some(self.changed_at)
    }

    fn best_tip_height_and_block_time(&self) -> Option<(block::Height, DateTime<Utc>)> {
        None
    }

    fn best_tip_mined_transaction_ids(&self) -> Arc<[transaction::Hash]> {
        Arc::new([])
    }

    async fn best_tip_changed(&mut self) -> Result<(), BoxError> {
        std::future::pending().await
    }

    fn mark_best_tip_seen(&mut self) {}
}

/// Each publication is recorded once, as the time since it was published, and a return to an
/// earlier tip is a new publication.
#[test]
fn records_each_publication_once() {
    const AGE: Duration = Duration::from_millis(50);

    let recorder = FirstTemplateRecorder::default();
    let _recorder_guard = metrics::set_default_local_recorder(&recorder);
    let first_template = FirstTemplateTip::default();
    let published = Instant::now()
        .checked_sub(AGE)
        .expect("the monotonic clock started more than 50 ms ago");
    let tip = |hash, changed_at| PublishedTip {
        hash,
        changed_at,
        pause: None,
    };
    let (a, b) = (block::Hash([1; 32]), block::Hash([2; 32]));

    assert!(first_template.record(&tip(a, published), a));
    assert!(!first_template.record(&tip(a, published), a));
    assert!(
        !first_template.record(&tip(b, published), a),
        "a template on a tip that is no longer the best tip isn't recorded",
    );
    assert!(first_template.record(&tip(b, published), b));

    let republished = published + Duration::from_millis(1);
    assert!(first_template.record(&tip(a, republished), a));

    let samples = recorder.samples();
    assert_eq!(samples.len(), 3, "one sample per recorded publication");
    for sample in samples {
        assert!(
            (AGE.as_secs_f64() - 0.001..10.0).contains(&sample),
            "{sample} is the time since the tip was published",
        );
    }
}

/// An RPC that read an older publication can't replace a newer one that another RPC recorded
/// while it was reading.
#[test]
fn a_stale_publication_does_not_replace_a_newer_one() {
    let first_template = FirstTemplateTip::default();
    let published = Instant::now();
    let (a, b) = (block::Hash([1; 32]), block::Hash([2; 32]));
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);

    let stale_tip = PublishedTip {
        hash: a,
        changed_at: published,
        pause: Some((started_tx, Mutex::new(release_rx))),
    };
    let stale_rpc = {
        let first_template = first_template.clone();
        thread::spawn(move || first_template.record(&stale_tip, a))
    };
    started_rx
        .recv()
        .expect("the stale RPC starts reading publication A");

    let current_tip = PublishedTip {
        hash: b,
        changed_at: published + Duration::from_millis(1),
        pause: None,
    };
    let current_rpc = {
        let first_template = first_template.clone();
        thread::spawn(move || first_template.record(&current_tip, b))
    };

    // Give the current RPC time to record B while the stale RPC is still reading A. Holding the
    // lock across the read makes it wait instead, so the result doesn't depend on this delay.
    thread::sleep(Duration::from_millis(100));
    release_tx.send(()).expect("the stale RPC is waiting");

    assert!(stale_rpc.join().expect("the stale RPC doesn't panic"));
    assert!(current_rpc.join().expect("the current RPC doesn't panic"));
    assert_eq!(
        *first_template.0.lock().expect("no thread panicked"),
        Some((b, published + Duration::from_millis(1))),
    );
}

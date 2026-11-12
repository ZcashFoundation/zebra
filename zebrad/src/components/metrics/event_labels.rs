//! Event attributes: labels whose keys start with [`EVENT_LABEL_PREFIX`].
//!
//! An event attribute can take a new value on every record, such as a block height, so a backend
//! that keeps a series or a value per label set must drop it. [`StripEventLabelsLayer`] does that
//! for Prometheus. Event attributes are only allowed on histograms.

use metrics::{
    Counter, Gauge, Histogram, Key, KeyName, Label, Metadata, Recorder, SharedString, Unit,
};
use metrics_util::layers::Layer;

/// The prefix of a label key that marks the label as an event attribute.
pub(crate) const EVENT_LABEL_PREFIX: &str = "event.";

/// Returns whether `label` is an event attribute.
pub(crate) fn is_event_label(label: &Label) -> bool {
    label.key().starts_with(EVENT_LABEL_PREFIX)
}

/// Returns `key` without its event attributes, and those attributes, or `None` if it has none.
///
/// Checks the labels before copying any, so a key without event attributes costs no allocation.
pub(crate) fn split_event_labels(key: &Key) -> Option<(Key, Vec<Label>)> {
    if !key.labels().any(is_event_label) {
        return None;
    }

    let (event_labels, labels): (Vec<Label>, Vec<Label>) =
        key.labels().cloned().partition(is_event_label);

    Some((Key::from_parts(key.name_shared(), labels), event_labels))
}

/// A [`Layer`] that drops event attributes before metrics reach the wrapped recorder.
#[derive(Clone, Copy, Debug, Default)]
#[cfg_attr(not(feature = "prometheus"), allow(dead_code))]
pub(crate) struct StripEventLabelsLayer;

impl<R> Layer<R> for StripEventLabelsLayer {
    type Output = StripEventLabels<R>;

    fn layer(&self, inner: R) -> Self::Output {
        StripEventLabels(inner)
    }
}

/// A recorder that passes metrics to the wrapped recorder without their event attributes.
#[derive(Debug)]
#[cfg_attr(not(feature = "prometheus"), allow(dead_code))]
pub(crate) struct StripEventLabels<R>(R);

#[cfg_attr(not(feature = "prometheus"), allow(dead_code))]
impl<R> StripEventLabels<R> {
    /// Calls `register` with `key`, or with `key` without its event attributes if it has any.
    fn with_key<T>(&self, key: &Key, register: impl FnOnce(&R, &Key) -> T) -> T {
        match split_event_labels(key) {
            Some((key, _)) => register(&self.0, &key),
            None => register(&self.0, key),
        }
    }
}

impl<R: Recorder> Recorder for StripEventLabels<R> {
    fn describe_counter(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
        self.0.describe_counter(key, unit, description);
    }

    fn describe_gauge(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
        self.0.describe_gauge(key, unit, description);
    }

    fn describe_histogram(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
        self.0.describe_histogram(key, unit, description);
    }

    fn register_counter(&self, key: &Key, metadata: &Metadata<'_>) -> Counter {
        self.with_key(key, |inner, key| inner.register_counter(key, metadata))
    }

    fn register_gauge(&self, key: &Key, metadata: &Metadata<'_>) -> Gauge {
        self.with_key(key, |inner, key| inner.register_gauge(key, metadata))
    }

    fn register_histogram(&self, key: &Key, metadata: &Metadata<'_>) -> Histogram {
        self.with_key(key, |inner, key| inner.register_histogram(key, metadata))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use metrics::NoopRecorder;

    use super::*;

    /// A recorder that keeps the key of each registered metric.
    #[derive(Default)]
    struct Registered(Mutex<Vec<Key>>);

    impl Registered {
        fn push(&self, key: &Key) {
            self.0
                .lock()
                .expect("the lock is only held to push, which can't panic")
                .push(key.clone());
        }
    }

    impl Recorder for Registered {
        fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

        fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

        fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

        fn register_counter(&self, key: &Key, metadata: &Metadata<'_>) -> Counter {
            self.push(key);
            NoopRecorder.register_counter(key, metadata)
        }

        fn register_gauge(&self, key: &Key, metadata: &Metadata<'_>) -> Gauge {
            self.push(key);
            NoopRecorder.register_gauge(key, metadata)
        }

        fn register_histogram(&self, key: &Key, metadata: &Metadata<'_>) -> Histogram {
            self.push(key);
            NoopRecorder.register_histogram(key, metadata)
        }
    }

    #[test]
    fn event_attributes_are_split_from_labels() {
        let key = Key::from_parts(
            "a.delay_seconds",
            vec![
                Label::new("event.chain.height", "3543405"),
                Label::new("chain.network_upgrade", "Nu7"),
            ],
        );

        let (stripped, event_labels) =
            split_event_labels(&key).expect("the key has an event attribute");

        assert_eq!(
            stripped,
            Key::from_parts(
                "a.delay_seconds",
                vec![Label::new("chain.network_upgrade", "Nu7")],
            ),
        );
        assert_eq!(event_labels, [Label::new("event.chain.height", "3543405")]);
        assert_eq!(
            split_event_labels(&stripped),
            None,
            "a key without event attributes is used as it is",
        );
    }

    #[test]
    fn the_layer_drops_event_attributes_from_every_kind() {
        let layered = StripEventLabelsLayer.layer(Registered::default());

        metrics::with_local_recorder(&layered, || {
            metrics::counter!("a.count", "event.chain.height" => "1", "kind" => "a").increment(1);
            metrics::gauge!("a.gauge", "event.chain.height" => "1").set(1.0);
            metrics::histogram!("a.delay_seconds", "event.chain.height" => "1").record(1.0);
            metrics::histogram!("a.duration_seconds", "result" => "success").record(1.0);
        });

        let StripEventLabels(Registered(registered)) = layered;
        let registered = registered
            .into_inner()
            .expect("the lock is only held to push, which can't panic");
        assert_eq!(
            registered,
            [
                Key::from_parts("a.count", vec![Label::new("kind", "a")]),
                Key::from_name("a.gauge"),
                Key::from_name("a.delay_seconds"),
                Key::from_parts("a.duration_seconds", vec![Label::new("result", "success")]),
            ],
        );
    }
}

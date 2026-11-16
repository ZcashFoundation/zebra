//! Block decode timing metrics.

use std::sync::atomic::{AtomicU64, Ordering};

use metrics::{
    Counter, Gauge, Histogram, HistogramFn, Key, KeyName, Metadata, Recorder, SharedString, Unit,
};

use super::*;

/// The sample count and the most recent sample of one histogram.
#[derive(Default)]
struct Samples {
    count: AtomicU64,
    last: AtomicU64,
}

impl Samples {
    fn count(&self) -> u64 {
        self.count.load(Ordering::SeqCst)
    }

    fn last(&self) -> f64 {
        f64::from_bits(self.last.load(Ordering::SeqCst))
    }
}

impl HistogramFn for Samples {
    fn record(&self, value: f64) {
        self.last.store(value.to_bits(), Ordering::SeqCst);
        self.count.fetch_add(1, Ordering::SeqCst);
    }
}

/// Records the block decode histograms and ignores every other metric.
#[derive(Default)]
struct DecodeRecorder {
    wait: Arc<Samples>,
    duration: Arc<Samples>,
}

impl Recorder for DecodeRecorder {
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
        let samples = match key.name() {
            "zebra.net.block.decode.wait_seconds" => &self.wait,
            "zebra.net.block.decode.duration_seconds" => &self.duration,
            _ => return Histogram::noop(),
        };
        assert_eq!(key.labels().count(), 0, "decode histograms are unlabeled");
        Histogram::from_arc(samples.clone())
    }
}

fn codec() -> Codec {
    Codec::builder()
        .with_max_body_len(MAX_PROTOCOL_MESSAGE_LEN)
        .finish()
}

fn encoded(message: Message) -> BytesMut {
    let mut bytes = BytesMut::new();
    codec()
        .encode(message, &mut bytes)
        .expect("test vectors encode");
    bytes
}

fn frame(command: &[u8; 12], body: &[u8]) -> BytesMut {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&Network::Mainnet.magic().0);
    bytes.extend_from_slice(command);
    bytes
        .write_u32::<LittleEndian>(u32::try_from(body.len()).unwrap())
        .unwrap();
    bytes.extend_from_slice(&sha256d::Checksum::from(body).0);
    bytes.extend_from_slice(body);
    BytesMut::from(bytes.as_slice())
}

/// Each received `block` message records its Rayon wait, and a decoded block also records the
/// whole decode, which contains that wait. Other transaction-bearing messages record neither.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn block_decode_records_wait_and_duration() {
    let _guard = zebra_test::init();
    let recorder = DecodeRecorder::default();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let mut codec = codec();

    let block_bytes = &*zebra_test::vectors::BLOCK_MAINNET_1_BYTES;
    let message = Message::Block(block_bytes.zcash_deserialize_into().unwrap());
    assert_eq!(
        codec.decode(&mut encoded(message.clone())).unwrap(),
        Some(message)
    );
    assert_eq!(recorder.wait.count(), 1);
    assert_eq!(recorder.duration.count(), 1);
    assert!(
        recorder.duration.last() >= recorder.wait.last(),
        "the decode duration starts before the Rayon wait and ends after the parse",
    );

    let tx: Transaction = zebra_test::vectors::DUMMY_TX1
        .zcash_deserialize_into()
        .unwrap();
    let message = Message::Tx(Arc::new(tx).into());
    assert_eq!(
        codec.decode(&mut encoded(message.clone())).unwrap(),
        Some(message)
    );
    assert_eq!(recorder.wait.count(), 1, "only block messages are timed");
    assert_eq!(
        recorder.duration.count(),
        1,
        "only block messages are timed"
    );

    // A truncated block body waits for the pool like any other, but never becomes a block.
    let truncated = &block_bytes[..block_bytes.len() / 2];
    assert!(codec
        .decode(&mut frame(b"block\0\0\0\0\0\0\0", truncated))
        .is_err());
    assert_eq!(recorder.wait.count(), 2);
    assert_eq!(
        recorder.duration.count(),
        1,
        "failed decodes record no duration"
    );
}

/// A block with enough transactions for the parallel decoder decodes to the same message, and
/// records both histograms. The decoder starts at the reader's position, and stops reading at
/// the end of the block.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn large_blocks_decode_in_the_rayon_pool() {
    let _guard = zebra_test::init();
    let recorder = DecodeRecorder::default();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let mut codec = codec();

    let block_bytes = zebra_chain::block::tests::generate::many_transaction_v4_block(200);
    let block: Arc<Block> = block_bytes.zcash_deserialize_into().unwrap();
    assert_eq!(block.transactions.len(), 200);
    let message = Message::Block(block);

    for _ in 0..2 {
        assert_eq!(
            codec.decode(&mut encoded(message.clone())).unwrap(),
            Some(message.clone())
        );
    }
    assert_eq!(recorder.wait.count(), 2);
    assert_eq!(recorder.duration.count(), 2);

    for before in [0, 5] {
        let mut body = vec![0xff; before];
        body.extend_from_slice(&block_bytes);
        body.extend_from_slice(&[0; 3]);
        let body = BytesMut::from(body.as_slice());
        let mut reader = Cursor::new(&body);
        reader.set_position(u64::try_from(before).unwrap());

        assert_eq!(codec.read_block(&mut reader).unwrap(), message);
        assert_eq!(
            reader.position(),
            u64::try_from(before + block_bytes.len()).unwrap(),
            "the reader is left at the end of the block",
        );
    }

    // A reader that is already at the end of its bytes has no block to decode.
    let body = BytesMut::from(block_bytes.as_slice());
    let mut reader = Cursor::new(&body);
    reader.set_position(u64::MAX);
    assert!(codec.read_block(&mut reader).is_err());
}

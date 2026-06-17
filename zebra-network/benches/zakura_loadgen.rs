//! Machine-readable Zakura single-process loadgen smoke.
#![allow(missing_docs)]
#![allow(clippy::print_stdout)]

use std::time::{Duration, Instant};

use zebra_network::zakura::Frame;

fn main() {
    let duration = Duration::from_secs(1);
    let payload = vec![0; 1024];
    let frame = Frame {
        message_type: 1,
        flags: 0,
        payload,
    };
    let max_frame_bytes = 2048;
    let start = Instant::now();
    let mut messages = 0u64;
    let mut bytes = 0u64;

    while start.elapsed() < duration {
        let encoded = frame.encode(max_frame_bytes).expect("loadgen frame fits");
        let decoded = Frame::decode(&encoded, max_frame_bytes).expect("loadgen frame decodes");
        messages = messages.saturating_add(1);
        bytes = bytes.saturating_add(decoded.payload.len() as u64);
    }
    let elapsed = start.elapsed();
    let elapsed_secs = elapsed.as_secs_f64();
    let messages_per_sec = messages as f64 / elapsed_secs;
    let bytes_per_sec = bytes as f64 / elapsed_secs;

    println!(
        "{{\"mode\":\"frame_codec\",\"duration_ms\":{},\"messages\":{},\"bytes\":{},\"messages_per_sec\":{},\"bytes_per_sec\":{}}}",
        elapsed.as_millis(),
        messages,
        bytes,
        messages_per_sec,
        bytes_per_sec
    );
}

//! Benchmarks for the JSON-RPC compatibility middleware's response round trip.
//!
//! `HttpRequestMiddleware` parses every response body and serializes it again. These benches
//! drive the real middleware over an inner service that returns a canned response, and compare
//! it with calling that service directly.
//!
//! # Test data
//!
//! Synthetic JSON-RPC 2.0 responses of about 2 MB and 50 MB, in two `result` shapes:
//! - `hex`: one long string, like `getblock` verbosity 0
//! - `objects`: `getblock` verbosity 2 transaction objects from mainnet block 1,687,118,
//!   repeated to the target size

// Disabled due to warnings in criterion macros
#![allow(missing_docs)]

use std::io::Cursor;

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use http_body_util::{BodyExt, Full};
use jsonrpsee::server::{HttpBody, HttpRequest, HttpResponse};
use tower::{service_fn, BoxError, Service, ServiceExt};

use zebra_chain::{block::Block, parameters::Network, serialization::ZcashDeserialize};
use zebra_rpc::{
    client::TransactionObject, server::http_request_compatibility::HttpRequestMiddleware,
};

const MB: usize = 1024 * 1024;

const REQUEST: &str = r#"{"jsonrpc":"2.0","method":"getblock","params":["1",2],"id":1}"#;

/// Wraps `result` in a JSON-RPC 2.0 response.
fn response(result: &str) -> Bytes {
    let body = format!(r#"{{"jsonrpc":"2.0","id":1,"result":{result}}}"#);
    // The middleware passes invalid JSON through untouched, so make sure the parse path runs.
    serde_json::from_str::<serde::de::IgnoredAny>(&body).expect("valid json");
    body.into()
}

/// A response with a hex string `result` of `size` bytes.
fn hex_response(size: usize) -> Bytes {
    response(&format!("\"{}\"", "ab".repeat(size / 2)))
}

/// A response with a `result` of verbosity 2 transaction objects, about `size` bytes long.
fn objects_response(size: usize) -> Bytes {
    let block = Block::zcash_deserialize(Cursor::new(
        zebra_test::vectors::BLOCK_MAINNET_1687118_BYTES.as_slice(),
    ))
    .expect("valid block");
    let objects: Vec<String> = block
        .transactions
        .iter()
        .map(|tx| {
            let object = TransactionObject::from_transaction(
                tx.clone(),
                block.coinbase_height(),
                Some(1),
                &Network::Mainnet,
                Some(block.header.time),
                Some(block.hash()),
                Some(true),
                tx.hash(),
            );
            serde_json::to_string(&object).expect("valid json")
        })
        .collect();

    let mut txs = Vec::new();
    let mut len = 0;
    while len < size {
        for object in &objects {
            len += object.len() + 1;
            txs.push(object.as_str());
        }
    }

    response(&format!(r#"{{"tx":[{}]}}"#, txs.join(",")))
}

/// An inner service that always returns `body`.
fn inner_service(
    body: Bytes,
) -> impl Service<HttpRequest, Response = HttpResponse, Error = BoxError, Future: Send>
       + Clone
       + Send
       + 'static {
    service_fn(move |_request: HttpRequest| {
        let body = body.clone();
        async move { Ok::<_, BoxError>(HttpResponse::new(HttpBody::new(Full::new(body)))) }
    })
}

/// Sends one request through `service` and collects the response body.
async fn round_trip<S>(service: &mut S) -> Bytes
where
    S: Service<HttpRequest, Response = HttpResponse>,
    S::Error: std::fmt::Debug,
{
    let response = service
        .ready()
        .await
        .expect("service is ready")
        .call(HttpRequest::new(HttpBody::from(REQUEST)))
        .await
        .expect("service returns a response");

    response
        .into_body()
        .collect()
        .await
        .expect("body is readable")
        .to_bytes()
}

fn bench_response_round_trip(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime builds");
    let mut group = c.benchmark_group("http compat response");
    group.sample_size(10);

    for size in [2 * MB, 50 * MB] {
        for (shape, body) in [
            ("hex", hex_response(size)),
            ("objects", objects_response(size)),
        ] {
            let label = format!("{shape} {} MB", body.len() / MB);
            group.throughput(Throughput::Bytes(body.len() as u64));

            let mut middleware = HttpRequestMiddleware::new(inner_service(body.clone()), None, MB);
            group.bench_function(BenchmarkId::new("middleware", &label), |b| {
                b.iter(|| runtime.block_on(round_trip(&mut middleware)))
            });

            // Baseline: the same inner service without the middleware.
            let mut direct = inner_service(body);
            group.bench_function(BenchmarkId::new("direct", &label), |b| {
                b.iter(|| runtime.block_on(round_trip(&mut direct)))
            });
        }
    }

    group.finish();
}

criterion_group!(benches, bench_response_round_trip);
criterion_main!(benches);

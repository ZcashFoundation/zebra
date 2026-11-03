//! Pins the raw `getblock` and `getblockheader` responses for the requests lightwalletd sends.
//!
//! lightwalletd syncs each block with `getblock ["<height>", 1]`, then `getblock ["<hash>", 0]`
//! with the hash from the first reply, in JSON-RPC 1.0 envelopes. These tests post those exact
//! bodies through the full server stack and snapshot the raw response bodies, so any change to the
//! bytes lightwalletd receives shows up as a snapshot diff or an assertion failure.
//!
//! The raw block, the raw header and the Equihash solutions are checked byte-for-byte against the
//! test vectors, then replaced by `[BlockData]`, `[HeaderData]` and `[Solution]` in the snapshot.
//!
//! To update the snapshots, run:
//! ```sh
//! cargo insta test --review -p zebra-rpc --lib -- get_block_lightwalletd_sync
//! ```

// `insta`'s macro expansion trips this lint, and each toolchain knows only one of its two names: https://github.com/rust-lang/rust/issues/79813
#![allow(unknown_lints)]
#![allow(semicolon_in_expressions_from_macros)]
#![allow(semicolon_in_expressions_from_non_local_macros)]

use std::{fmt::Write as _, net::SocketAddr, sync::Arc};

use hex::ToHex;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::watch;
use tower::buffer::Buffer;

use zebra_chain::{
    block::{Block, Height},
    chain_sync_status::MockSyncStatus,
    parameters::Network::{self, Mainnet},
    serialization::{ZcashDeserializeInto, ZcashSerialize},
};
use zebra_network::address_book_peers::MockAddressBookPeers;
use zebra_node_services::BoxError;
use zebra_test::mock_service::MockService;

use crate::methods::RpcImpl;

use super::harness;

/// The fields lightwalletd reads from a verbosity 1 reply, shaped like its Go struct
/// `ZcashRpcReplyGetblock1` (`common/common.go`).
///
/// Go fills missing keys with zero values, so `trees` and its members are optional here as well.
/// lightwalletd needs `hash` for its second call and one `tx` entry per transaction, so those are
/// required.
#[derive(Debug, Deserialize)]
struct LightwalletdGetblock1 {
    hash: String,
    tx: Vec<String>,
    #[serde(default)]
    trees: LightwalletdTrees,
}

/// The `trees` object of [`LightwalletdGetblock1`].
#[derive(Debug, Default, Deserialize)]
struct LightwalletdTrees {
    #[serde(default)]
    sapling: LightwalletdTreeSize,
    #[serde(default)]
    orchard: LightwalletdTreeSize,
}

/// A note commitment tree size, which lightwalletd stores as a Go `uint32`.
#[derive(Debug, Default, Deserialize)]
struct LightwalletdTreeSize {
    #[serde(default)]
    size: u32,
}

/// Sends requests the way lightwalletd's RPC client does, and records them with their responses.
struct LightwalletdClient {
    /// The RPC server's address.
    addr: SocketAddr,

    /// The id of the next request. btcd's `rpcclient` numbers requests from 1.
    next_id: u64,

    /// Each request and its response, in order, for the snapshot.
    transcript: String,
}

impl LightwalletdClient {
    fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            next_id: 1,
            transcript: String::new(),
        }
    }

    /// Posts `method` with the literal JSON `params`, and returns the request body, its id and the
    /// raw response body.
    ///
    /// The body is the JSON-RPC 1.0 envelope btcd's `rpcclient.RawRequest` marshals, with its
    /// `btcjson.Request` field order.
    async fn call(&mut self, method: &str, params: &str) -> (String, u64, String) {
        let id = self.next_id;
        self.next_id += 1;

        let request =
            format!(r#"{{"jsonrpc":"1.0","method":"{method}","params":{params},"id":{id}}}"#);
        let response = harness::post(self.addr, &request).await;

        (request, id, response)
    }

    /// Appends a request and the response to record for it to the transcript.
    fn record(&mut self, request: &str, response: &str) {
        writeln!(self.transcript, "--> {request}\n<-- {response}\n")
            .expect("writing to a String does not fail");
    }
}

/// Parses a JSON-RPC 1.0 response envelope with the expected `id`, and returns its `result` and
/// `error`.
fn parse_legacy_response(response: &str, id: u64) -> (Value, Value) {
    let response: Value = serde_json::from_str(response).expect("the server responds with JSON");

    assert_eq!(
        response["jsonrpc"], "1.0",
        "a 1.0 request gets a 1.0 response"
    );
    assert_eq!(response["id"], id, "the response echoes the request id");

    let response = response
        .as_object()
        .expect("a JSON-RPC response is an object");
    let result = response
        .get("result")
        .expect("a 1.0 response always has a result")
        .clone();
    let error = response
        .get("error")
        .expect("a 1.0 response always has an error")
        .clone();

    (result, error)
}

/// Replaces the JSON string `"<hex>"` in `response` with `"[<label>]"`, after checking it occurs
/// exactly once.
///
/// Used for hex payloads the caller has already checked byte-for-byte against the test vectors,
/// to keep the snapshot readable.
fn redact_checked_hex(response: &str, hex: &str, label: &str) -> String {
    let quoted = format!("\"{hex}\"");
    assert_eq!(
        response.matches(&quoted).count(),
        1,
        "the checked hex appears once in the response"
    );

    response.replace(&quoted, &format!("\"[{label}]\""))
}

/// Checks that the `solution` of a verbose block or header `result` is the Equihash solution at
/// the end of the test vector's `header_bytes`, then redacts it in `response`.
fn redact_checked_solution(
    response: &str,
    result: &Value,
    block: &Block,
    header_bytes: &[u8],
) -> String {
    let solution: String = block.header.solution.encode_hex();
    assert!(
        header_bytes.ends_with(&hex::decode(&solution).expect("hex encoding round trips")),
        "a header ends with its Equihash solution"
    );
    assert_eq!(
        result["solution"], solution,
        "the verbose solution is the test vector's solution"
    );

    redact_checked_hex(response, &solution, "Solution")
}

/// lightwalletd's sync requests get byte-identical responses on Mainnet.
#[tokio::test(flavor = "multi_thread")]
async fn get_block_lightwalletd_sync_mainnet() {
    get_block_lightwalletd_sync(Mainnet).await;
}

/// lightwalletd's sync requests get byte-identical responses on Testnet.
#[tokio::test(flavor = "multi_thread")]
async fn get_block_lightwalletd_sync_testnet() {
    get_block_lightwalletd_sync(Network::new_default_testnet()).await;
}

/// Posts lightwalletd's sync requests for every test vector block of `network`, checks the
/// responses the way lightwalletd reads them, and snapshots the raw responses.
async fn get_block_lightwalletd_sync(network: Network) {
    let _init_guard = zebra_test::init();

    let block_data = network.blockchain_map();
    let blocks: Vec<Arc<Block>> = block_data
        .values()
        .map(|block_bytes| {
            block_bytes
                .zcash_deserialize_into()
                .expect("test vectors are valid blocks")
        })
        .collect();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let (state, read_state, tip, _) = zebra_state::populated_state(blocks.clone(), &network).await;

    let (_tx, rx) = watch::channel(None);
    let (rpc, rpc_tx_queue) = RpcImpl::new(
        network.clone(),
        Default::default(),
        false,
        "0.0.1",
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        state,
        read_state,
        Buffer::new(MockService::build().for_unit_tests::<_, _, BoxError>(), 1),
        MockSyncStatus::default(),
        tip,
        MockAddressBookPeers::default(),
        rx,
        None,
    );

    let (addr, server_task) = harness::spawn(rpc).await;
    let mut client = LightwalletdClient::new(addr);

    for ((&height, &block_bytes), block) in block_data.iter().zip(&blocks) {
        let header_bytes = block
            .header
            .zcash_serialize_to_vec()
            .expect("serializing a valid header to a Vec does not fail");
        assert!(
            block_bytes.starts_with(&header_bytes),
            "a block starts with its header"
        );

        // lightwalletd's first call: verbosity 1 by height, with the height as a JSON string.
        let (request, id, response) = client.call("getblock", &format!(r#"["{height}",1]"#)).await;

        let (result, error) = parse_legacy_response(&response, id);
        assert_eq!(error, Value::Null);

        client.record(
            &request,
            &redact_checked_solution(&response, &result, block, &header_bytes),
        );

        let reply: LightwalletdGetblock1 =
            serde_json::from_value(result).expect("lightwalletd can parse the verbose reply");
        assert_eq!(reply.hash, block.hash().to_string());
        assert_eq!(
            reply.tx,
            block
                .transactions
                .iter()
                .map(|tx| tx.hash().to_string())
                .collect::<Vec<_>>(),
        );
        // lightwalletd copies the tree sizes into its compact block metadata. The test vectors
        // are all below Sapling activation, so both trees are empty.
        assert_eq!(reply.trees.sapling.size, 0);
        assert_eq!(reply.trees.orchard.size, 0);

        // lightwalletd's second call: verbosity 0 by the hash from the first reply.
        let (request, id, response) = client
            .call("getblock", &format!(r#"["{}",0]"#, reply.hash))
            .await;

        let (result, error) = parse_legacy_response(&response, id);
        assert_eq!(error, Value::Null);

        let block_hex = result.as_str().expect("verbosity 0 returns a hex string");
        let raw_block = hex::decode(block_hex).expect("verbosity 0 returns valid hex");
        assert_eq!(
            raw_block, block_bytes,
            "verbosity 0 returns the test vector's bytes"
        );

        // lightwalletd rejects a block whose coinbase height differs from the requested height.
        let raw_block: Block = raw_block
            .zcash_deserialize_into()
            .expect("the bytes equal a valid test vector");
        assert_eq!(raw_block.coinbase_height(), Some(Height(height)));

        client.record(
            &request,
            &redact_checked_hex(&response, block_hex, "BlockData"),
        );

        // `getblockheader` is not on lightwalletd's sync path, but shares the code path that
        // serves `getblock` verbosity 1.
        let (request, id, response) = client
            .call("getblockheader", &format!(r#"["{}",false]"#, block.hash()))
            .await;

        let (result, error) = parse_legacy_response(&response, id);
        assert_eq!(error, Value::Null);

        let header_hex = result
            .as_str()
            .expect("a non-verbose header is a hex string");
        assert_eq!(
            hex::decode(header_hex).expect("a non-verbose header is valid hex"),
            header_bytes,
            "a non-verbose header is the test vector's header bytes"
        );

        client.record(
            &request,
            &redact_checked_hex(&response, header_hex, "HeaderData"),
        );

        let (request, id, response) = client
            .call("getblockheader", &format!(r#"["{height}",true]"#))
            .await;

        let (result, error) = parse_legacy_response(&response, id);
        assert_eq!(error, Value::Null);
        assert_eq!(result["hash"], block.hash().to_string());
        assert_eq!(result["height"], height);

        client.record(
            &request,
            &redact_checked_solution(&response, &result, block, &header_bytes),
        );
    }

    // lightwalletd polls the next height with verbosity 1, and reads an error whose code is `-8`
    // as "not mined yet". Its RPC client formats errors as "<code>: <message>" and lightwalletd
    // compares the text before the first ':'.
    let next_height = block_data
        .keys()
        .last()
        .expect("the test vectors are not empty")
        + 1;
    let (request, id, response) = client
        .call("getblock", &format!(r#"["{next_height}",1]"#))
        .await;
    client.record(&request, &response);

    let (result, error) = parse_legacy_response(&response, id);
    assert_eq!(result, Value::Null);
    let rpc_error = format!(
        "{}: {}",
        error["code"].as_i64().expect("an error code is an integer"),
        error["message"]
            .as_str()
            .expect("an error message is a string"),
    );
    assert_eq!(
        rpc_error.split(':').next(),
        Some("-8"),
        "lightwalletd reads -8 as a height that is not mined yet: {rpc_error}"
    );

    // A verbosity above 3 is rejected by the handler, not by parameter parsing.
    let (request, id, response) = client.call("getblock", r#"["1",4]"#).await;
    client.record(&request, &response);

    let (result, error) = parse_legacy_response(&response, id);
    assert_eq!(result, Value::Null);
    assert_eq!(error["code"], -8);
    assert_eq!(error["message"], "invalid verbosity value");

    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_suffix(format!(
        "{}_{}",
        network.to_string().to_ascii_lowercase(),
        blocks.len() - 1
    ));
    settings.bind(|| insta::assert_snapshot!("get_block_lightwalletd_sync", client.transcript));

    mempool.expect_no_requests().await;

    server_task.abort();
    rpc_tx_queue.abort();
}

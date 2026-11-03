//! Checks how the RPC server decodes the second `getblock` argument as clients send it.
//!
//! A parameter that fails to parse is `-32602` inside the `RpcModule`, and clients see `-1`,
//! because the server's middleware rewrites the code. Named parameters are decoded by a struct
//! that jsonrpsee generates from the trait's parameter names and that ignores unknown keys. So
//! these tests post literal request bodies through the full server stack.

use std::{collections::BTreeSet, net::SocketAddr, sync::Arc};

use serde_json::{json, Value};
use tokio::sync::watch;
use tower::buffer::Buffer;

use zebra_chain::{
    block::Block, chain_sync_status::MockSyncStatus, chain_tip::NoChainTip,
    parameters::Network::Mainnet, serialization::ZcashDeserializeInto,
};
use zebra_network::address_book_peers::MockAddressBookPeers;
use zebra_node_services::BoxError;
use zebra_test::mock_service::MockService;

use crate::methods::{types::get_block::GetBlockQuery, RpcImpl, RpcServer as _};

use super::harness;

/// Posts a JSON-RPC 2.0 `getblock` request with the literal JSON `params`, and returns the
/// response's `result` and `error`, which are `Value::Null` when absent.
async fn post_getblock(addr: SocketAddr, params: &str) -> (Value, Value) {
    let request = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"getblock","params":{params}}}"#);
    let response = harness::post(addr, &request).await;
    let response: Value = serde_json::from_str(&response)
        .unwrap_or_else(|error| panic!("the server responds to {params} with JSON: {error}"));

    assert_eq!(response["id"], 1, "the response echoes the request id");

    (response["result"].clone(), response["error"].clone())
}

/// Every accepted form of the second argument reaches the handler as the query it means.
#[tokio::test(flavor = "multi_thread")]
async fn getblock_accepts_every_query_form() {
    let _init_guard = zebra_test::init();

    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .values()
        .map(|block_bytes| {
            block_bytes
                .zcash_deserialize_into()
                .expect("test vectors are valid blocks")
        })
        .collect();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let (state, read_state, tip, _) = zebra_state::populated_state(blocks, &Mainnet).await;

    let (_tx, rx) = watch::channel(None);
    let (rpc, rpc_tx_queue) = RpcImpl::new(
        Mainnet,
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

    let (addr, server_task) = harness::spawn(rpc.clone()).await;

    // The expected results, from direct calls that bypass parameter decoding.
    let direct = |query: GetBlockQuery| {
        let rpc = &rpc;
        async move {
            serde_json::to_value(
                rpc.get_block("1".to_owned(), Some(query.clone()))
                    .await
                    .unwrap_or_else(|error| panic!("{query:?} of block 1 failed: {error:?}")),
            )
            .expect("getblock responses serialize")
        }
    };
    let verbosity_0 = direct(GetBlockQuery::Verbosity(0)).await;
    let verbosity_1 = direct(GetBlockQuery::Verbosity(1)).await;
    assert!(verbosity_0.is_string(), "verbosity 0 is the raw block");
    assert!(verbosity_1.is_object(), "verbosity 1 is an object");
    let hash = verbosity_1["hash"].clone();

    for (params, expected) in [
        // A missing or null argument is verbosity 1.
        (r#"["1"]"#, verbosity_1.clone()),
        (r#"["1",null]"#, verbosity_1.clone()),
        (r#"["1",1]"#, verbosity_1.clone()),
        // Booleans are verbosity 1 and 0, as in zcashd.
        (r#"["1",true]"#, verbosity_1.clone()),
        (r#"["1",false]"#, verbosity_0.clone()),
        (
            r#"["1",["hash","tx","trees"]]"#,
            json!({
                "hash": hash,
                "tx": verbosity_1["tx"],
                "trees": verbosity_1["trees"],
            }),
        ),
        // `txids` is an alias of `tx`.
        (
            r#"["1",["txids"]]"#,
            json!({ "hash": hash, "tx": verbosity_1["tx"] }),
        ),
        // Named parameters use the trait's parameter names. If the second one were not called
        // `verbosity`, this would silently be verbosity 1.
        (
            r#"{"hash_or_height":"1","verbosity":["hash"]}"#,
            json!({ "hash": hash }),
        ),
    ] {
        let (result, error) = post_getblock(addr, params).await;

        assert_eq!(error, Value::Null, "{params}");
        assert_eq!(result, expected, "{params}");

        if let Some(object) = result.as_object() {
            let keys: BTreeSet<&String> = object.keys().collect();
            let expected_keys: BTreeSet<&String> = expected
                .as_object()
                .expect("an object is expected for an object result")
                .keys()
                .collect();
            assert_eq!(keys, expected_keys, "{params}");
        }
    }

    mempool.expect_no_requests().await;

    server_task.abort();
    rpc_tx_queue.abort();
}

/// Second arguments that are not a verbosity, a boolean or a list of field names fail parameter
/// parsing with zcashd's `-1 "Invalid params"`, with the reason in `data`, before the handler
/// reads the state.
#[tokio::test(flavor = "multi_thread")]
async fn getblock_rejects_malformed_queries_as_invalid_params() {
    let _init_guard = zebra_test::init();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut read_state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut block_verifier_router: MockService<_, _, _, BoxError> =
        MockService::build().for_unit_tests();

    let (_tx, rx) = watch::channel(None);
    let (rpc, rpc_tx_queue) = RpcImpl::new(
        Mainnet,
        Default::default(),
        false,
        "0.0.1",
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        Buffer::new(state.clone(), 1),
        Buffer::new(read_state.clone(), 1),
        Buffer::new(block_verifier_router.clone(), 1),
        MockSyncStatus::default(),
        NoChainTip,
        MockAddressBookPeers::default(),
        rx,
        None,
    );

    let (addr, server_task) = harness::spawn(rpc).await;

    const EXPECTED_QUERY: &str =
        "expected a verbosity level from 0 to 3, a boolean, or an array of getblock field names";

    for (params, reasons) in [
        (
            r#"["1","1"]"#,
            &[r#"invalid type: string "1""#, EXPECTED_QUERY][..],
        ),
        (
            r#"["1",256]"#,
            &["invalid value: integer `256`", EXPECTED_QUERY],
        ),
        (
            r#"["1",-1]"#,
            &["invalid value: integer `-1`", EXPECTED_QUERY],
        ),
        (
            r#"["1",1.5]"#,
            &["invalid type: floating point `1.5`", EXPECTED_QUERY],
        ),
        (r#"["1",{}]"#, &["invalid type: map", EXPECTED_QUERY]),
        (
            r#"["1",["merkelroot"]]"#,
            &["unknown variant `merkelroot`", "expected one of"],
        ),
        (
            r#"["1",[1]]"#,
            &[
                "invalid type: integer `1`",
                "expected a getblock field name",
            ],
        ),
    ] {
        let (result, error) = post_getblock(addr, params).await;

        assert_eq!(result, Value::Null, "{params}");
        assert_eq!(error["code"], -1, "{params}: {error}");
        assert_eq!(error["message"], "Invalid params", "{params}: {error}");

        let data = error["data"]
            .as_str()
            .unwrap_or_else(|| panic!("{params}: the parse error is the data: {error}"));
        for reason in reasons {
            assert!(data.contains(reason), "{params}: {data:?} has {reason:?}");
        }
    }

    mempool.expect_no_requests().await;
    state.expect_no_requests().await;
    read_state.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;

    server_task.abort();
    rpc_tx_queue.abort();
}

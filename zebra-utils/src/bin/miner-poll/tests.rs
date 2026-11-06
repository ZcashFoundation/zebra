//! Tests for `miner-poll`.

use std::sync::Arc;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use zebra_chain::{
    amount::Amount,
    block,
    parameters::{Network, NetworkUpgrade},
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transaction::Transaction,
};
use zebra_rpc::{
    client::TransactionTemplate,
    config::mining::{default_miner_address, Config, MinerAddressType},
    MinerParams,
};
use zebra_test::vectors::{BLOCK_MAINNET_1_BYTES, BLOCK_MAINNET_2_BYTES, BLOCK_MAINNET_3_BYTES};

use super::*;

/// Returns a poll with questions written as for `--options`.
fn poll(election: &str, questions: &str) -> Poll {
    Poll {
        election: election.to_owned(),
        questions: questions
            .split(',')
            .map(|question| parse_question(question).expect("valid question"))
            .collect(),
    }
}

/// Returns answers written as for `--answers`.
fn answers(answers: &str) -> Answers {
    answers
        .split(',')
        .map(|answer| parse_answer(answer).expect("valid answer"))
        .collect()
}

/// The first Election of the ZIP's example: five single-choice questions with 4, 4, 4, 3, and 4
/// options.
fn nu8() -> Poll {
    poll("nu8", "4,4,4,3,4")
}

/// The second Election of the ZIP's example: a multiple-choice question with 6 options and at most
/// 3 selections, and a single-choice question with 3 options.
fn devfund() -> Poll {
    poll("devfund", "6:3,3")
}

/// The ZIP's example tables of Miner Data.
#[test]
fn zip_ballot_vectors() {
    let valid = Some(answers("2,3,1,1,-"));

    for (data, expected) in [
        ("zpoll:nu8:2311-", valid.clone()),
        ("/ExamplePool/ zpoll:nu8:2311-", valid.clone()),
        ("zpoll:nu8:2311-/ExamplePool/", valid.clone()),
        ("zpoll:nu8:9 zpoll:nu8:2311-", valid),
        ("zpoll:nu8:2311", None),
        ("zpoll:nu8:2311-X", None),
        ("zpoll:nu8:2311-(ExamplePool)", None),
        ("zpoll:nu8:2314-", None),
        ("zpoll:nu8:-----", None),
        ("ZPOLL:nu8:2311-", None),
        ("zpoll:nu7:2311-", None),
    ] {
        assert_eq!(find_ballot(data.as_bytes(), &nu8()), expected, "{data}");
    }

    for (data, expected) in [
        ("zpoll:devfund:(135)2", Some(answers("1+3+5,2"))),
        ("zpoll:devfund:(3)-", Some(answers("3,-"))),
        ("zpoll:devfund:-2", Some(answers("-,2"))),
        ("zpoll:devfund:3-", None),
        ("zpoll:devfund:(1)(2)", None),
        ("zpoll:devfund:()2", None),
        ("zpoll:devfund:(1356)2", None),
        ("zpoll:devfund:(31)2", None),
        ("zpoll:devfund:(33)2", None),
        ("zpoll:devfund:(17)2", None),
    ] {
        assert_eq!(find_ballot(data.as_bytes(), &devfund()), expected, "{data}");
    }

    // Letters select options 10 to 35.
    let wide = poll("e", "35,10,12:2");
    assert_eq!(
        find_ballot(b"zpoll:e:ZA(AC)", &wide),
        Some(answers("35,10,10+12"))
    );
    assert_eq!(find_ballot(b"zpoll:e:AB-", &wide), None);
    assert_eq!(find_ballot(b"zpoll:e:ZA(AD)", &wide), None);
}

/// Encoded answers form the Miner Ballot the tally reads back, and invalid answers are rejected
/// with their reason.
#[test]
fn encoded_ballots_read_back() {
    for (poll, answers, ballot) in [
        (nu8(), answers("2,3,1,1,-"), "zpoll:nu8:2311-"),
        (devfund(), answers("1+3+5,2"), "zpoll:devfund:(135)2"),
        (devfund(), answers("3,-"), "zpoll:devfund:(3)-"),
        (
            poll("e", "35,10,9,4,12:2"),
            answers("35,10,9,-,10+12"),
            "zpoll:e:ZA9-(AC)",
        ),
    ] {
        assert_eq!(encode(&answers, &poll).as_deref(), Ok(ballot));
        assert_eq!(find_ballot(ballot.as_bytes(), &poll), Some(answers));
    }

    for (poll, answers, error) in [
        (
            nu8(),
            answers("1"),
            InvalidBallot::Length {
                answers: 1,
                questions: 5,
            },
        ),
        (nu8(), answers("-,-,-,-,-"), InvalidBallot::Unanswered),
        (
            nu8(),
            answers("2,3,1,4,-"),
            InvalidBallot::Option {
                question: 4,
                options: 3,
            },
        ),
        (
            nu8(),
            answers("2,1+3,1,1,-"),
            InvalidBallot::Selections {
                question: 2,
                max: 1,
            },
        ),
        (
            devfund(),
            answers("1+3+5+6,2"),
            InvalidBallot::Selections {
                question: 1,
                max: 3,
            },
        ),
        (
            devfund(),
            answers("1+7,2"),
            InvalidBallot::Option {
                question: 1,
                options: 6,
            },
        ),
        (
            devfund(),
            answers("3+1,2"),
            InvalidBallot::Order { question: 1 },
        ),
        (
            devfund(),
            answers("3+3,2"),
            InvalidBallot::Order { question: 1 },
        ),
    ] {
        assert_eq!(encode(&answers, &poll), Err(error));
    }
}

/// A question allows 1 to 35 options, and a multiple-choice question 2 selections up to its
/// number of options.
#[test]
fn question_definitions() {
    for (question, expected) in [
        ("1", Some((1, None))),
        ("35", Some((35, None))),
        ("6:2", Some((6, Some(2)))),
        ("6:6", Some((6, Some(6)))),
        ("0", None),
        ("36", None),
        ("6:1", None),
        ("6:7", None),
        ("6:", None),
    ] {
        let question = parse_question(question).ok();
        let parsed = question.map(|question| (question.options, question.max_selections));
        assert_eq!(parsed, expected);
    }
}

/// Every Miner Ballot of an Election fits in a 512-byte memo, and Elections whose longest Miner
/// Ballot doesn't fit are rejected.
#[test]
fn longest_ballots_fit_in_a_memo() {
    // 13 multiple-choice questions with 37-character answers, and 8 single-choice questions.
    let questions = [vec!["35:35"; 13], vec!["1"; 8]].concat().join(",");
    let longest = poll("abcdefghijklmnop", &questions);
    longest.check_size().expect("the longest ballot fits");

    let all_options = (1..=35)
        .map(|o| o.to_string())
        .collect::<Vec<_>>()
        .join("+");
    let all_answers = [vec![all_options.as_str(); 13], vec!["1"; 8]]
        .concat()
        .join(",");
    let ballot = encode(&answers(&all_answers), &longest).expect("valid answers");
    assert_eq!(ballot.len(), 512);

    assert!(poll("e", &format!("{questions},1")).check_size().is_err());
    assert!(poll("e", &vec!["2"; 489].join(",")).check_size().is_ok());
    assert!(poll("e", &vec!["2"; 490].join(",")).check_size().is_err());
}

/// `--answers` accepts a list that starts with an unanswered question, and several options joined
/// by `+`.
#[test]
fn answers_can_start_unanswered() {
    let args = Args::try_parse_from([
        "miner-poll",
        "encode",
        "--election",
        "e",
        "--options",
        "2,3:2",
        "--answers",
        "-,1+3",
    ])
    .expect("a leading `-` is an answer, not a flag");

    let Command::Encode { poll, answers } = args.command else {
        panic!("parsed as another command: {args:?}");
    };
    assert_eq!(answers, vec![vec![], vec![1, 3]]);
    assert_eq!(encode(&answers, &poll).as_deref(), Ok("zpoll:e:-(13)"));
}

/// Only ZIP 302 text memos carry Miner Ballots, and zero padding ends the answer string.
#[test]
fn text_memos() {
    let memo = |bytes: &[u8]| {
        let mut memo = [0; 512];
        memo[..bytes.len()].copy_from_slice(bytes);
        memo
    };

    let padded = memo(b"zpoll:nu8:2311-");
    assert!(is_text_memo(&padded));
    assert!(find_ballot(&padded, &nu8()).is_some());

    // Not valid UTF-8 without the trailing zero bytes.
    assert!(!is_text_memo(&memo(b"zpoll:nu8:2311-\xff")));
    // A first byte above 0xF4 marks a non-text memo, even if the rest is a ballot.
    assert!(!is_text_memo(&memo(b"\xf5zpoll:nu8:2311-")));
}

/// Percentages are relative to all blocks in the Voting Period, and each question is assessed
/// against the Participation Threshold separately, as in the ZIP's examples.
#[test]
fn tally_results() {
    let poll = nu8();
    let ballot = |data: &[u8]| find_ballot(data, &poll).expect("valid ballot");

    let mut few = Tally::new(&poll);
    for _ in 0..10 {
        few.add(&ballot(b"zpoll:nu8:2311-"));
    }
    let results = few.results(100_000, 0.5);
    assert_eq!(results["participation_percentage"], json!(0.01));
    assert_eq!(results["questions"][0]["options"][1]["votes"], json!(10));
    assert_eq!(
        results["questions"][0]["options"][1]["percentage"],
        json!(0.01)
    );
    assert_eq!(results["questions"][0]["not_answered"], json!(99_990));
    assert_eq!(results["questions"][0]["representative"], json!(false));

    let mut partial = Tally::new(&poll);
    for _ in 0..99_990 {
        partial.add(&ballot(b"zpoll:nu8:2311-"));
    }
    for _ in 0..10 {
        partial.add(&ballot(b"zpoll:nu8:23113"));
    }
    let results = partial.results(100_000, 0.5);
    assert_eq!(results["participation_percentage"], json!(100.0));
    for question in 0..4 {
        assert_eq!(
            results["questions"][question]["representative"],
            json!(true)
        );
    }
    assert_eq!(results["questions"][4]["options"][2]["votes"], json!(10));
    assert_eq!(results["questions"][4]["not_answered"], json!(99_990));
    assert_eq!(results["questions"][4]["representative"], json!(false));
}

/// A block that selects several options of a question votes for each of them, but answers the
/// question once.
#[test]
fn multiple_choice_tally_results() {
    let poll = devfund();
    let ballot = |data: &[u8]| find_ballot(data, &poll).expect("valid ballot");

    let mut tally = Tally::new(&poll);
    for _ in 0..30 {
        tally.add(&ballot(b"zpoll:devfund:(135)2"));
    }
    for _ in 0..20 {
        tally.add(&ballot(b"zpoll:devfund:(3)-"));
    }
    let results = tally.results(100, 50.0);

    let q1 = &results["questions"][0];
    assert_eq!(q1["options"][0]["votes"], json!(30));
    assert_eq!(q1["options"][2]["votes"], json!(50));
    assert_eq!(q1["options"][2]["percentage"], json!(50.0));
    assert_eq!(q1["options"][4]["votes"], json!(30));
    assert_eq!(q1["not_answered"], json!(50));
    assert_eq!(q1["representative"], json!(true));

    let q2 = &results["questions"][1];
    assert_eq!(q2["not_answered"], json!(70));
    assert_eq!(q2["representative"], json!(false));
}

/// Miner Ballots cast with Zebra's `extra_coinbase_data` and `miner_memo` options are found in
/// the coinbase transactions Zebra builds, with the Miner Data taking precedence.
#[test]
fn ballots_in_zebra_coinbase_transactions() {
    let network = Network::new_default_testnet();
    let height = NetworkUpgrade::Nu6_3
        .activation_height(&network)
        .expect("NU6.3 is scheduled on Testnet");
    let miner_address = default_miner_address(network.kind(), &MinerAddressType::Unified);

    let coinbase = |extra_coinbase_data: Option<&str>| -> Transaction {
        let config: Config = serde_json::from_value(json!({
            "miner_address": miner_address,
            "extra_coinbase_data": extra_coinbase_data,
            "miner_memo": "zpoll:nu8:1111-",
        }))
        .expect("valid mining config");
        let miner_params = MinerParams::new(&network, config).expect("valid miner params");

        TransactionTemplate::new_coinbase(&network, height, &miner_params, Amount::zero())
            .expect("valid coinbase transaction")
            .data()
            .as_ref()
            .zcash_deserialize_into()
            .expect("Zebra builds valid coinbase transactions")
    };

    assert_eq!(
        coinbase_ballot(
            &coinbase(Some("/Pool/ zpoll:nu8:2311-")),
            &network,
            height,
            &nu8()
        ),
        Some(answers("2,3,1,1,-")),
        "the Miner Data ballot takes precedence over the memo ballot",
    );
    assert_eq!(
        coinbase_ballot(&coinbase(None), &network, height, &nu8()),
        Some(answers("1,1,1,1,-")),
        "the memo ballot counts without a Miner Data ballot",
    );
}

/// Starts a fake node that answers each JSON-RPC request with `respond(method, params)`, and
/// returns its RPC address.
async fn fake_node(respond: impl Fn(&str, &Value) -> Value + Send + 'static) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a local port is free");
    let addr = listener.local_addr().expect("the listener is bound");

    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut request = Vec::new();
            let body = loop {
                let mut buf = [0; 4096];
                let read = stream
                    .read(&mut buf)
                    .await
                    .expect("the client sends a request");
                assert_ne!(read, 0, "the client closed the connection mid-request");
                request.extend_from_slice(&buf[..read]);

                let Some(headers) = request.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let length: usize = String::from_utf8_lossy(&request[..headers])
                    .to_lowercase()
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:")?.trim().parse().ok())
                    .unwrap_or(0);
                if let Some(body) = request.get(headers + 4..headers + 4 + length) {
                    break body.to_vec();
                }
            };

            let request: Value = serde_json::from_slice(&body).expect("a JSON-RPC request");
            let method = request["method"].as_str().expect("a method name");
            let response = json!({
                "jsonrpc": "2.0",
                "id": request["id"],
                "result": respond(method, &request["params"]),
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{response}",
                response.len()
            );
            // The client stops reading a response that is too long.
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });

    addr
}

/// Tallies heights 1 to 3 on `network` from a fake node whose genesis block is `genesis`, whose
/// tip is at height `tip`, which serves `blocks` for heights 1 to 3, and which reports
/// `confirmations` for the block at height 3.
async fn tally_fake_node(
    network: &Network,
    genesis: block::Hash,
    tip: u32,
    blocks: Vec<Block>,
    confirmations: i64,
) -> Result<Value> {
    let addr = fake_node(move |method, params| match method {
        "getblockhash" => json!(genesis.to_string()),
        "getblockchaininfo" => json!({ "blocks": tip }),
        "getblockheader" => json!({ "confirmations": confirmations }),
        "getblock" => {
            let height: usize = params[0]
                .as_str()
                .and_then(|h| h.parse().ok())
                .expect("height");
            let block = blocks[height - 1].zcash_serialize_to_vec();
            json!(hex::encode(block.expect("valid block")))
        }
        _ => panic!("unexpected method {method}"),
    })
    .await;

    tally(
        &nu8(),
        network,
        Height(1),
        Height(3),
        50.0,
        &Rpc::new(addr, None),
    )
    .await
}

/// Mainnet blocks 1 to 3.
fn mainnet_blocks() -> Vec<Block> {
    [
        &*BLOCK_MAINNET_1_BYTES,
        &*BLOCK_MAINNET_2_BYTES,
        &*BLOCK_MAINNET_3_BYTES,
    ]
    .map(|bytes| bytes.zcash_deserialize_into().expect("valid block"))
    .to_vec()
}

/// Testnet and Regtest nodes report the same chain name, so a tally checks the genesis block.
#[tokio::test]
async fn tally_rejects_other_networks() {
    let regtest_genesis = Network::new_regtest(Default::default()).genesis_hash();

    let error = tally_fake_node(
        &Network::new_default_testnet(),
        regtest_genesis,
        3,
        mainnet_blocks(),
        1,
    )
    .await
    .expect_err("a Regtest node is not a Testnet node");
    assert!(error.to_string().contains("not on"), "{error}");
}

/// A tally counts only blocks of one chain, and fails if the node's best chain changes during
/// the scan.
#[tokio::test]
async fn tally_rejects_chain_changes() {
    let genesis = Network::Mainnet.genesis_hash();
    let blocks = mainnet_blocks();

    let results = tally_fake_node(&Network::Mainnet, genesis, 3, blocks.clone(), 1)
        .await
        .expect("an unchanged chain tallies");
    assert_eq!(
        results["voting_end_block_hash"],
        json!(blocks[2].hash().to_string())
    );
    assert_eq!(results["blocks"], json!(3));

    // A reorg between two `getblock` calls.
    let mut forked = blocks.clone();
    Arc::make_mut(&mut forked[1].header).previous_block_hash = block::Hash([0; 32]);
    let error = tally_fake_node(&Network::Mainnet, genesis, 3, forked, 1)
        .await
        .expect_err("blocks from two chains");
    assert!(error.to_string().contains("changed"), "{error}");

    // A reorg after the last `getblock` call.
    let error = tally_fake_node(&Network::Mainnet, genesis, 3, blocks, -1)
        .await
        .expect_err("the scanned chain is no longer the best chain");
    assert!(error.to_string().contains("changed"), "{error}");
}

/// A tally is final only once the tallied block at the Voting End Height has 100 confirmations,
/// even if the node's tip height was 99 blocks above it when the tally started.
#[tokio::test]
async fn final_tallies_need_100_confirmations() {
    let genesis = Network::Mainnet.genesis_hash();

    for (confirmations, is_final) in [(99, false), (100, true)] {
        let results = tally_fake_node(
            &Network::Mainnet,
            genesis,
            102,
            mainnet_blocks(),
            confirmations,
        )
        .await
        .expect("an unchanged chain tallies");
        assert_eq!(results["final"], json!(is_final), "{confirmations}");
    }
}

/// The default cookie is only sent to a node on this machine; an explicit cookie directory is used
/// for any node.
#[test]
fn default_cookie_stays_on_this_machine() {
    let addr = |addr: &str| addr.parse().expect("valid address");
    let explicit = PathBuf::from("/cookies");

    assert_eq!(
        cookie_dir_for(addr("127.0.0.1:8232"), None),
        Some(default_cache_dir())
    );
    assert_eq!(
        cookie_dir_for(addr("[::1]:8232"), None),
        Some(default_cache_dir())
    );
    assert_eq!(cookie_dir_for(addr("192.0.2.1:8232"), None), None);
    assert_eq!(
        cookie_dir_for(addr("192.0.2.1:8232"), Some(explicit.clone())),
        Some(explicit)
    );
}

/// A response longer than a hex-encoded maximum-size block fails the call instead of being
/// buffered.
#[tokio::test]
async fn rpc_responses_are_bounded() {
    let addr = fake_node(|_, _| json!("0".repeat(MAX_RESPONSE_LEN))).await;

    let error = Rpc::new(addr, None)
        .call("getblockhash", json!([0]))
        .await
        .expect_err("the response is too long");
    assert!(error.to_string().contains("longer than"), "{error}");
}

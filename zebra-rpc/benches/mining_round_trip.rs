//! Benchmarks for the mining round trip through the `getblocktemplate` and `submitblock` RPCs.
//!
//! The situation measured is a miner that receives a template with mempool transactions, solves it
//! without modifying it, and submits the solved block. For each mempool size, three durations are
//! reported:
//!
//! - `tip_to_full_template`: from the previous block's commit to the first served template with
//!   transactions,
//! - `submit_solved_template`: `submitblock` latency for a block built from that template, which
//!   returns once the block is in the non-finalized state,
//! - `tip_to_commit`: both together.
//!
//! The node is real apart from the mempool: a Regtest network with proof of work disabled, an
//! ephemeral state, and the verifier router. The mempool is a table of pre-built transparent
//! transactions keyed by chain tip. Each committed block's transactions are spent again by the next
//! block's, so every round trip has a fresh, valid mempool. The transactions pay to `OP_TRUE`
//! scripts, so no signing is needed, and they are not verified by a mempool, so a submission that
//! misses the proposal cache pays full transaction verification.
//!
//! Run with `cargo bench -p zebra-rpc --bench mining_round_trip`. Set `ZEBRA_BENCH_SOLVE_MS` to
//! the milliseconds a solve is assumed to take: the submission waits that long after the template
//! is served, outside the timed region, so the template's background check can finish first as it
//! does for real solves. The default of zero submits immediately.

#![allow(missing_docs)]

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use tokio::sync::{broadcast, watch};
use tower::ServiceExt;

use zcash_address::{ToAddress, ZcashAddress};
use zebra_chain::{
    block::{self, genesis::regtest_genesis_block, Block, Height},
    chain_sync_status::MockSyncStatus,
    parameters::{
        testnet::{ConfiguredActivationHeights, RegtestParameters},
        Network, NetworkUpgrade,
    },
    serialization::ZcashSerialize,
    transaction::{LockTime, Transaction, VerifiedUnminedTx},
    transparent::{self, MIN_TRANSPARENT_COINBASE_MATURITY},
};
use zebra_network::address_book_peers::MockAddressBookPeers;
use zebra_node_services::mempool::{self, MempoolTxSubscriber};
use zebra_rpc::{
    client::{
        BlockTemplateTimeSource, GetBlockTemplateCapability, GetBlockTemplateParameters,
        GetBlockTemplateRequestMode, HexData, SubmitBlockResponse,
    },
    config::mining,
    methods::{RpcImpl, RpcServer},
    proposal_block_from_template,
};
use zebra_state::{ReadRequest, ReadResponse};

/// Mempool sizes to benchmark, in transactions. Every transaction is selected into the template.
const MEMPOOL_SIZES: [usize; 2] = [100, 1_000];

/// The assumed solve time between receiving a template and submitting it, from
/// `ZEBRA_BENCH_SOLVE_MS`.
fn solve_delay() -> Duration {
    std::env::var("ZEBRA_BENCH_SOLVE_MS")
        .ok()
        .and_then(|millis| millis.parse().ok())
        .map_or(Duration::ZERO, Duration::from_millis)
}

/// The ZIP-317 marginal fee, which prices one logical action.
const MARGINAL_FEE: i64 = 5_000;

/// The ZIP-317 grace actions: transactions pay for at least this many logical actions.
const GRACE_ACTIONS: usize = 2;

/// Coinbase outputs the benchmark spends to fund its transactions.
///
/// Their rewards are spread over the mempool transactions, and each round trip spends one
/// conventional fee per transaction, so this bounds the number of round trips: 100 Regtest
/// coinbases fund thousands of round trips at either mempool size.
const FUNDING_COINBASES: u32 = 100;

/// The `OP_TRUE` script, which spends with an empty signature script.
const OP_TRUE: [u8; 1] = [0x51];

/// `RIPEMD160(SHA256(OP_TRUE))`: the pay-to-script-hash of the `OP_TRUE` redeem script.
///
/// The coinbase pays this address, so block rewards can be spent without signing.
const OP_TRUE_SCRIPT_HASH: [u8; 20] = [
    0xda, 0x17, 0x45, 0xe9, 0xb5, 0x49, 0xbd, 0x0b, 0xfa, 0x1a, 0x56, 0x99, 0x71, 0xc7, 0x7e, 0xba,
    0x30, 0xcd, 0x5a, 0x4b,
];

/// The lock script of the coinbase outputs: `OP_HASH160 <OP_TRUE_SCRIPT_HASH> OP_EQUAL`.
fn coinbase_lock_script() -> transparent::Script {
    let mut script = vec![0xa9, 0x14];
    script.extend_from_slice(&OP_TRUE_SCRIPT_HASH);
    script.push(0x87);
    transparent::Script::new(&script)
}

/// The unlock script for the coinbase outputs: a push of the `OP_TRUE` redeem script.
fn coinbase_unlock_script() -> transparent::Script {
    transparent::Script::new(&[0x01, 0x51])
}

/// An output the benchmark can spend.
#[derive(Clone)]
struct Funds {
    outpoint: transparent::OutPoint,
    output: transparent::Output,
}

/// Builds a verified mempool transaction spending `inputs` into `outputs`.
///
/// The fee is whatever `inputs` carry beyond `outputs`, which callers size to the ZIP-317
/// conventional fee, so every transaction is selected without unpaid actions.
fn spend(
    inputs: &[Funds],
    unlock_script: &transparent::Script,
    outputs: Vec<transparent::Output>,
) -> VerifiedUnminedTx {
    let total_in: i64 = inputs
        .iter()
        .map(|funds| funds.output.value().zatoshis())
        .sum();
    let total_out: i64 = outputs.iter().map(|output| output.value().zatoshis()).sum();
    let fee = total_in - total_out;
    assert!(
        fee >= conventional_fee(inputs.len(), outputs.len()),
        "benchmark funds are exhausted: mine more funding coinbases"
    );
    let transaction = Transaction::test_v5(
        NetworkUpgrade::Nu5,
        inputs
            .iter()
            .map(|funds| transparent::Input::PrevOut {
                outpoint: funds.outpoint,
                unlock_script: unlock_script.clone(),
                sequence: u32::MAX,
            })
            .collect(),
        outputs,
        LockTime::unlocked(),
        Height(0),
    );
    VerifiedUnminedTx::new(
        Arc::new(transaction).into(),
        fee.try_into().expect("the fee is non-negative"),
        0,
        0,
        Arc::new(inputs.iter().map(|funds| funds.output.clone()).collect()),
    )
    .expect("transparent transactions have conventional fees")
}

/// The ZIP-317 conventional fee of a transparent transaction.
fn conventional_fee(inputs: usize, outputs: usize) -> i64 {
    let actions = inputs.max(outputs).max(GRACE_ACTIONS);
    MARGINAL_FEE * i64::try_from(actions).expect("action counts fit i64")
}

/// Spends every output of `block`'s non-coinbase transactions, one transaction per output.
fn spend_block_outputs(block: &Block) -> Vec<VerifiedUnminedTx> {
    let lock_script = transparent::Script::new(&OP_TRUE);
    let unlock_script = transparent::Script::new(&[]);
    block
        .transactions
        .iter()
        .skip(1)
        .flat_map(|transaction| {
            let hash = transaction.hash();
            transaction
                .outputs()
                .into_iter()
                .enumerate()
                .map(move |(index, output)| Funds {
                    outpoint: transparent::OutPoint {
                        hash,
                        index: u32::try_from(index).expect("output counts fit u32"),
                    },
                    output,
                })
        })
        .map(|funds| {
            let value = funds.output.value().zatoshis() - conventional_fee(1, 1);
            spend(
                std::slice::from_ref(&funds),
                &unlock_script,
                vec![transparent::Output::new(
                    value.try_into().expect("benchmark funds are exhausted"),
                    lock_script.clone(),
                )],
            )
        })
        .collect()
}

/// The mempool: pre-built transactions for the current chain tip and the block about to be
/// committed.
#[derive(Default)]
struct Mempool(
    Mutex<(
        VecDeque<block::Hash>,
        HashMap<block::Hash, Arc<Vec<VerifiedUnminedTx>>>,
    )>,
);

impl Mempool {
    /// The transactions that are valid on top of `tip`.
    fn for_tip(&self, tip: block::Hash) -> Arc<Vec<VerifiedUnminedTx>> {
        let (_, sets) = &*self.0.lock().expect("no panics hold the mempool lock");
        sets.get(&tip).cloned().unwrap_or_default()
    }

    /// Sets the transactions that are valid on top of `tip`, retaining only the previous set.
    fn set(&self, tip: block::Hash, transactions: Vec<VerifiedUnminedTx>) {
        let (order, sets) = &mut *self.0.lock().expect("no panics hold the mempool lock");
        order.push_back(tip);
        sets.insert(tip, Arc::new(transactions));
        while order.len() > 2 {
            let old = order.pop_front().expect("more than two entries");
            sets.remove(&old);
        }
    }
}

/// Starts a Regtest node with an ephemeral state, the verifier router, a mining RPC over
/// `mempool`, and the RPC's template updater.
async fn mining_node(mempool: Arc<Mempool>) -> (impl RpcServer + Clone, Network) {
    let network = Network::new_regtest(RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            nu5: Some(1),
            ..Default::default()
        },
        should_allow_unshielded_coinbase_spends: Some(true),
        ..Default::default()
    });
    let (state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::populated_state([regtest_genesis_block()], &network).await;

    let mempool_service = tower::service_fn({
        let read_state = read_state.clone();
        move |request| {
            let read_state = read_state.clone();
            let mempool = mempool.clone();
            async move {
                // Answer for the state's tip, which the template updater read just before this.
                let ReadResponse::Tip(Some((_, tip))) =
                    read_state.oneshot(ReadRequest::Tip).await?
                else {
                    return Err("the benchmark state has a tip".into());
                };
                let transactions = mempool.for_tip(tip);
                Ok::<_, zebra_node_services::BoxError>(match request {
                    mempool::Request::FullTransactions => mempool::Response::FullTransactions {
                        transactions: (*transactions).clone(),
                        transaction_dependencies: Default::default(),
                        last_seen_tip_hash: tip,
                    },
                    mempool::Request::TransactionIds => mempool::Response::TransactionIds(
                        transactions.iter().map(|tx| tx.transaction.id).collect(),
                    ),
                    other => panic!("unexpected mempool request: {other:?}"),
                })
            }
        }
    });

    let (verifier, _, _, _, prepared_blocks) = zebra_consensus::router::init_test(
        zebra_consensus::Config {
            checkpoint_sync: false,
        },
        &network,
        state.clone(),
        read_state.clone(),
    )
    .await;

    let mut sync_status = MockSyncStatus::default();
    sync_status.set_is_close_to_tip(true);
    let (_logs, logs) = watch::channel(None);
    let (rpc, _queue) = RpcImpl::new(
        network.clone(),
        mining::Config {
            miner_address: Some(ZcashAddress::from_transparent_p2sh(
                network.kind().into(),
                OP_TRUE_SCRIPT_HASH,
            )),
            ..Default::default()
        },
        false,
        "0.0.1",
        "RPC bench",
        mempool_service,
        state,
        read_state,
        verifier,
        sync_status,
        latest_chain_tip,
        MockAddressBookPeers::default(),
        logs,
        None,
    );
    let rpc = rpc.with_prepared_blocks(prepared_blocks);
    let (mempool_changes, _) = broadcast::channel(1);
    rpc.spawn_block_template_updater(&MempoolTxSubscriber::new(mempool_changes))
        .expect("mining is configured");

    (rpc, network)
}

/// Waits for a template with `transactions` transactions, builds a block from it unchanged, and
/// submits it. Before submitting, prepares the mempool for the new tip by spending the block's
/// transaction outputs.
///
/// Returns the block, the time until the template was served, and the `submitblock` latency.
async fn round_trip<Rpc: RpcServer>(
    rpc: &Rpc,
    network: &Network,
    mempool: &Mempool,
    transactions: usize,
) -> (Block, Duration, Duration) {
    let started = Instant::now();
    let mut parameters = None;
    let template = loop {
        let template = rpc
            .get_block_template(parameters.take())
            .await
            .expect("getblocktemplate succeeds")
            .try_into_template()
            .expect("template mode");
        let served = template.transactions().len();
        assert!(
            served == 0 || served == transactions,
            "templates select the whole benchmark mempool: {served} of {transactions}"
        );
        if served == transactions {
            break template;
        }
        // Long poll on the coinbase-only template until the full one is served.
        parameters = Some(GetBlockTemplateParameters::new(
            GetBlockTemplateRequestMode::Template,
            None,
            vec![
                GetBlockTemplateCapability::LongPoll,
                GetBlockTemplateCapability::CoinbaseTxn,
            ],
            Some(template.long_poll_id()),
            None,
        ));
    };
    let template_served = Instant::now();

    let block = proposal_block_from_template(&template, BlockTemplateTimeSource::CurTime, network)
        .expect("served templates convert to blocks");
    if block.transactions.len() > 1 {
        mempool.set(block.hash(), spend_block_outputs(&block));
    }
    let data = block.zcash_serialize_to_vec().expect("blocks serialize");
    let delay = solve_delay();
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }

    let submitted = Instant::now();
    let response = rpc
        .submit_block(HexData(data), None)
        .await
        .expect("submitblock succeeds");
    let committed = Instant::now();
    assert!(
        matches!(response, SubmitBlockResponse::Accepted),
        "the solved template must be accepted: {response:?}"
    );

    (block, template_served - started, committed - submitted)
}

/// Mines the funding coinbases and lets them mature, then spreads their rewards over
/// `mempool_size` outputs in one more block, so the next template holds `mempool_size` spends.
async fn fund<Rpc: RpcServer>(
    rpc: &Rpc,
    network: &Network,
    mempool: &Mempool,
    mempool_size: usize,
) {
    let lock_script = coinbase_lock_script();
    let mut coinbases = Vec::new();
    let mut tip = None;
    for _ in 0..FUNDING_COINBASES + MIN_TRANSPARENT_COINBASE_MATURITY {
        let (block, _, _) = round_trip(rpc, network, mempool, 0).await;
        tip = Some(block.hash());
        if coinbases.len() < FUNDING_COINBASES as usize {
            let coinbase = &block.transactions[0];
            let (index, output) = coinbase
                .outputs()
                .into_iter()
                .enumerate()
                .find(|(_, output)| output.lock_script == lock_script)
                .expect("the coinbase pays the miner address");
            coinbases.push(Funds {
                outpoint: transparent::OutPoint {
                    hash: coinbase.hash(),
                    index: u32::try_from(index).expect("output counts fit u32"),
                },
                output,
            });
        }
    }
    let total: i64 = coinbases
        .iter()
        .map(|funds| funds.output.value().zatoshis())
        .sum();
    let value = (total - conventional_fee(coinbases.len(), mempool_size))
        / i64::try_from(mempool_size).expect("mempool sizes fit i64");
    let fan_out = spend(
        &coinbases,
        &coinbase_unlock_script(),
        vec![
            transparent::Output::new(
                value.try_into().expect("funding rewards cover the fan-out"),
                transparent::Script::new(&OP_TRUE),
            );
            mempool_size
        ],
    );
    mempool.set(tip.expect("funding mines blocks"), vec![fan_out]);
    round_trip(rpc, network, mempool, 1).await;
}

/// Which part of the round trip a benchmark measures.
#[derive(Clone, Copy)]
enum Measure {
    /// From the previous commit to the first served template with transactions.
    Template,
    /// The `submitblock` call for the solved template.
    Submit,
    /// Both.
    Total,
}

fn mining_round_trip(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let mut group = c.benchmark_group("mining_round_trip");
    group
        .sample_size(10)
        .warm_up_time(Duration::from_secs(3))
        .measurement_time(Duration::from_secs(20));

    for mempool_size in MEMPOOL_SIZES {
        let mempool = Arc::new(Mempool::default());
        let (rpc, network) = runtime.block_on(mining_node(mempool.clone()));
        runtime.block_on(fund(&rpc, &network, &mempool, mempool_size));
        for (name, measure) in [
            ("tip_to_full_template", Measure::Template),
            ("submit_solved_template", Measure::Submit),
            ("tip_to_commit", Measure::Total),
        ] {
            group.bench_with_input(
                BenchmarkId::new(name, mempool_size),
                &mempool_size,
                |b, &mempool_size| {
                    b.to_async(&runtime).iter_custom(|iterations| {
                        let rpc = rpc.clone();
                        let network = network.clone();
                        let mempool = mempool.clone();
                        async move {
                            let mut total = Duration::ZERO;
                            for _ in 0..iterations {
                                let (_, template, submit) =
                                    round_trip(&rpc, &network, &mempool, mempool_size).await;
                                total += match measure {
                                    Measure::Template => template,
                                    Measure::Submit => submit,
                                    Measure::Total => template + submit,
                                };
                            }
                            total
                        }
                    })
                },
            );
        }
    }
    group.finish();
}

criterion_group!(benches, mining_round_trip);
criterion_main!(benches);

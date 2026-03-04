//! Test-format-based tests

#![cfg(test)]

use chrono::{DateTime, Utc};
use std::{path::PathBuf, sync::Arc, time::Duration};

use zcash_keys::address::Address;
use zebra_chain::{
    block::{
        Block, FatPointerToBftBlock, Hash as BlockHash, Header as BlockHeader,
        Height as BlockHeight,
    },
    fmt::HexDebug,
    history_tree::HistoryTree,
    orchard,
    parameters::Network,
    sapling,
    serialization::*,
    work::{self, difficulty::CompactDifficulty},
};
use zebra_chain::crosslink::*;
use zebra_state::crosslink::*;
use zebrad::components::crosslink::test_format::*;
use zebrad::application::CROSSLINK_TEST_CONFIG_OVERRIDE;
use zebrad::config::ZebradConfig;
use zebrad::prelude::Application;

macro_rules! function_name {
    () => {{
        fn f() {}
        fn type_name_of<T>(_: T) -> &'static str {
            std::any::type_name::<T>()
        }
        let name = type_name_of(f);
        name.strip_suffix("::f")
            .unwrap()
            .split("::")
            .last()
            .unwrap()
    }};
}

/// Set the global state TEST_NAME
pub fn set_test_name(name: &'static str) {
    *zebrad::components::crosslink::TEST_NAME.lock().unwrap() = name;
}

/// Crosslink Test entrypoint
pub fn test_start() {
    // init globals
    {
        *CROSSLINK_TEST_CONFIG_OVERRIDE.lock().unwrap() = {
            let mut base = ZebradConfig::default();
            base.network.network = Network::new_regtest(
                zebra_chain::parameters::testnet::RegtestParameters::default(),
            );
            base.state.ephemeral = true;

            Some(std::sync::Arc::new(base))
        };
        *zebrad::components::crosslink::TEST_MODE.lock().unwrap() = true;
        *zebra_chain::crosslink::BFT_HASH_USE_UNKEYED.lock().unwrap() = true;
        *zebrad::components::crosslink::TEST_SHUTDOWN_FN.lock().unwrap() = || {
            zebrad::components::crosslink::dump_test_instrs();
            // APPLICATION.shutdown(abscissa_core::Shutdown::Graceful);
            std::process::exit(*zebrad::components::crosslink::TEST_FAILED.lock().unwrap());
        }
    }

    use zebrad::application::{ZebradApp, APPLICATION};

    // boot
    let os_args: Vec<_> = std::env::args_os().collect();
    // panic!("OS args: {:?}", os_args);
    let args: Vec<std::ffi::OsString> = vec![
        os_args[0].clone(),
        zebrad::commands::EntryPoint::default_cmd_as_str().into(),
    ];
    // println!("args: {:?}", args);

    ZebradApp::run(&APPLICATION, args);
}

/// Run a Crosslink Test from a dynamic byte array.
pub fn test_bytes(bytes: Vec<u8>) {
    *zebrad::components::crosslink::TEST_INSTR_BYTES.lock().unwrap() = bytes;
    test_start();
}

/// Run a Crosslink Test from a file path.
pub fn test_path(path: PathBuf) {
    *zebrad::components::crosslink::TEST_INSTR_PATH.lock().unwrap() = Some(path);
    test_start();
}

#[ignore]
#[test]
fn read_from_file() {
    test_path("../crosslink-test-data/blocks.zeccltf".into());
}

const REGTEST_BLOCK_BYTES: &[&[u8]] = &[
    //                                                                     i   h
    include_bytes!("../../crosslink-test-data/test_pow_block_0.bin"), //   0,  1: 02a610...
    include_bytes!("../../crosslink-test-data/test_pow_block_1.bin"), //   1,  2: 02d711...
    include_bytes!("../../crosslink-test-data/test_pow_block_2.bin"), //   2,  fork 3: 098207...
    include_bytes!("../../crosslink-test-data/test_pow_block_3.bin"), //   3,  3: 0032241...
    include_bytes!("../../crosslink-test-data/test_pow_block_4.bin"), //   4,  4: 0247f1a...
    include_bytes!("../../crosslink-test-data/test_pow_block_6.bin"), //   5,  5: 0ab3a5d8...
    include_bytes!("../../crosslink-test-data/test_pow_block_7.bin"), //   6,  6
    include_bytes!("../../crosslink-test-data/test_pow_block_9.bin"), //   7,  7
    include_bytes!("../../crosslink-test-data/test_pow_block_10.bin"), //  8,  8
    include_bytes!("../../crosslink-test-data/test_pow_block_12.bin"), //  9,  9
    include_bytes!("../../crosslink-test-data/test_pow_block_13.bin"), // 10, 10
    include_bytes!("../../crosslink-test-data/test_pow_block_14.bin"), // 11, 11
    include_bytes!("../../crosslink-test-data/test_pow_block_15.bin"), // 12, 12
    include_bytes!("../../crosslink-test-data/test_pow_block_17.bin"), // 13, 13
    include_bytes!("../../crosslink-test-data/test_pow_block_18.bin"), // 14, 14
    include_bytes!("../../crosslink-test-data/test_pow_block_19.bin"), // 15, 15
    include_bytes!("../../crosslink-test-data/test_pow_block_21.bin"), // 16, 16
    include_bytes!("../../crosslink-test-data/test_pow_block_22.bin"), // 17, 17
    include_bytes!("../../crosslink-test-data/test_pow_block_23.bin"), // 18, 18
    include_bytes!("../../crosslink-test-data/test_pow_block_25.bin"), // 19, 19
    include_bytes!("../../crosslink-test-data/test_pow_block_26.bin"), // 20, 20
    include_bytes!("../../crosslink-test-data/test_pow_block_28.bin"), // 21, 21
    include_bytes!("../../crosslink-test-data/test_pow_block_29.bin"), // 22, 22
];
const REGTEST_BLOCK_BYTES_N: usize = REGTEST_BLOCK_BYTES.len();

fn regtest_block_hashes() -> [BlockHash; REGTEST_BLOCK_BYTES_N] {
    let mut hashes = [BlockHash([0; 32]); REGTEST_BLOCK_BYTES_N];
    for i in 0..REGTEST_BLOCK_BYTES_N {
        // ALT: BlockHeader::
        hashes[i] = Block::zcash_deserialize(REGTEST_BLOCK_BYTES[i])
            .unwrap()
            .hash();
    }
    hashes
}

const REGTEST_POS_BLOCK_BYTES: &[&[u8]] = &[
    include_bytes!("../../crosslink-test-data/test_pos_block_5.bin"),
    include_bytes!("../../crosslink-test-data/test_pos_block_8.bin"),
    include_bytes!("../../crosslink-test-data/test_pos_block_11.bin"),
    include_bytes!("../../crosslink-test-data/test_pos_block_16.bin"),
    include_bytes!("../../crosslink-test-data/test_pos_block_20.bin"),
    include_bytes!("../../crosslink-test-data/test_pos_block_24.bin"),
    include_bytes!("../../crosslink-test-data/test_pos_block_27.bin"),
];

const REGTEST_POW_IDX_FINALIZED_BY_POS_BLOCK: &[usize] = &[1, 4, 6, 10, 13, 16, 18];

#[test]
fn crosslink_expect_pos_height_on_boot() {
    set_test_name(function_name!());
    let mut tf = TF::new(&PROTOTYPE_PARAMETERS);

    tf.push_instr_expect_pos_chain_length(0, 0);

    test_bytes(tf.write_to_bytes());
}

#[test]
fn crosslink_expect_pow_height_on_boot() {
    set_test_name(function_name!());
    let mut tf = TF::new(&PROTOTYPE_PARAMETERS);

    tf.push_instr_expect_pow_chain_length(1, 0);

    test_bytes(tf.write_to_bytes());
}

#[test]
fn crosslink_expect_first_pow_to_not_be_a_no_op() {
    set_test_name(function_name!());
    let mut tf = TF::new(&PROTOTYPE_PARAMETERS);

    tf.push_instr_load_pow_bytes(REGTEST_BLOCK_BYTES[0], 0);
    tf.push_instr_expect_pow_chain_length(2, 0);

    test_bytes(tf.write_to_bytes());
}

#[test]
fn crosslink_push_example_pow_chain_only() {
    set_test_name(function_name!());
    let mut tf = TF::new(&PROTOTYPE_PARAMETERS);

    for i in 0..REGTEST_BLOCK_BYTES.len() {
        tf.push_instr_load_pow_bytes(REGTEST_BLOCK_BYTES[i], 0);
        tf.push_instr_expect_pow_chain_length(2 + i - (i >= 3) as usize, 0);
    }
    tf.push_instr_expect_pow_chain_length(1 - 1 + REGTEST_BLOCK_BYTES.len(), 0);

    test_bytes(tf.write_to_bytes());
}

#[test]
fn crosslink_push_example_pow_chain_each_block_twice() {
    set_test_name(function_name!());
    let mut tf = TF::new(&PROTOTYPE_PARAMETERS);

    for i in 0..REGTEST_BLOCK_BYTES.len() {
        tf.push_instr_load_pow_bytes(REGTEST_BLOCK_BYTES[i], 0);
        tf.push_instr_load_pow_bytes(REGTEST_BLOCK_BYTES[i], SHOULD_FAIL);
        tf.push_instr_expect_pow_chain_length(2 + i - (i >= 3) as usize, 0);
    }
    tf.push_instr_expect_pow_chain_length(1 - 1 + REGTEST_BLOCK_BYTES.len(), 0);

    test_bytes(tf.write_to_bytes());
}

#[test]
fn crosslink_push_example_pow_chain_again_should_not_change_the_pow_chain_length() {
    set_test_name(function_name!());
    let mut tf = TF::new(&PROTOTYPE_PARAMETERS);

    for i in 0..REGTEST_BLOCK_BYTES.len() {
        tf.push_instr_load_pow_bytes(REGTEST_BLOCK_BYTES[i], 0);
        tf.push_instr_expect_pow_chain_length(2 + i - (i >= 3) as usize, 0);
    }
    tf.push_instr_expect_pow_chain_length(1 - 1 + REGTEST_BLOCK_BYTES.len(), 0);

    for i in 0..REGTEST_BLOCK_BYTES.len() {
        tf.push_instr_load_pow_bytes(REGTEST_BLOCK_BYTES[i], SHOULD_FAIL);
        tf.push_instr_expect_pow_chain_length(1 - 1 + REGTEST_BLOCK_BYTES.len(), 0);
    }

    test_bytes(tf.write_to_bytes());
}

#[test]
fn crosslink_expect_pos_not_pushed_if_pow_blocks_not_present() {
    set_test_name(function_name!());
    let mut tf = TF::new(&PROTOTYPE_PARAMETERS);

    tf.push_instr_load_pos_bytes(REGTEST_POS_BLOCK_BYTES[0], SHOULD_FAIL);
    tf.push_instr_expect_pos_chain_length(0, 0);

    test_bytes(tf.write_to_bytes());
}

#[test]
fn crosslink_expect_pos_height_after_push() {
    set_test_name(function_name!());
    let mut tf = TF::new(&PROTOTYPE_PARAMETERS);

    for i in 0..REGTEST_BLOCK_BYTES.len() {
        tf.push_instr_load_pow_bytes(REGTEST_BLOCK_BYTES[i], 0);
    }
    for i in 0..REGTEST_POS_BLOCK_BYTES.len() {
        tf.push_instr_load_pos_bytes(REGTEST_POS_BLOCK_BYTES[i], 0);
        tf.push_instr_expect_pos_chain_length(1 + i, 0);
    }

    // let write_ok = tf.write_to_file(Path::new("crosslink_expect_pos_height_after_push.zeccltf"));
    // assert!(write_ok);
    test_bytes(tf.write_to_bytes());
}

#[test]
fn crosslink_expect_pos_out_of_order() {
    set_test_name(function_name!());
    let mut tf = TF::new(&PROTOTYPE_PARAMETERS);

    for i in 0..5 {
        tf.push_instr_load_pow_bytes(REGTEST_BLOCK_BYTES[i], 0);
    }

    tf.push_instr_load_pos_bytes(REGTEST_POS_BLOCK_BYTES[0], 0);
    tf.push_instr_load_pos_bytes(REGTEST_POS_BLOCK_BYTES[2], SHOULD_FAIL);
    tf.push_instr_load_pos_bytes(REGTEST_POS_BLOCK_BYTES[1], 0);
    tf.push_instr_expect_pos_chain_length(2, 0);

    test_bytes(tf.write_to_bytes());
}

#[test]
fn crosslink_expect_pos_push_same_block_twice_only_accepted_once() {
    set_test_name(function_name!());
    let mut tf = TF::new(&PROTOTYPE_PARAMETERS);

    for i in 0..REGTEST_BLOCK_BYTES.len() {
        tf.push_instr_load_pow_bytes(REGTEST_BLOCK_BYTES[i], 0);
    }
    tf.push_instr_load_pos_bytes(REGTEST_POS_BLOCK_BYTES[0], 0);
    tf.push_instr_load_pos_bytes(REGTEST_POS_BLOCK_BYTES[0], SHOULD_FAIL);
    tf.push_instr_expect_pos_chain_length(1, 0);

    test_bytes(tf.write_to_bytes());
}

#[test]
fn crosslink_reject_pos_with_signature_on_different_data() {
    set_test_name(function_name!());
    let mut tf = TF::new(&PROTOTYPE_PARAMETERS);

    for i in 0..REGTEST_BLOCK_BYTES.len() {
        tf.push_instr_load_pow_bytes(REGTEST_BLOCK_BYTES[i], 0);
    }

    // modify block data so that the signatures are incorrect
    // NOTE: modifying the last as there is no `previous_block_hash` that this invalidates
    let mut bft_block_and_fat_ptr =
        BftBlockAndFatPointerToIt::zcash_deserialize(REGTEST_POS_BLOCK_BYTES[0]).unwrap();
    // let mut last_pow_hdr =
    bft_block_and_fat_ptr.block.headers.last_mut().unwrap().time += Duration::from_secs(1);
    let new_bytes = bft_block_and_fat_ptr.zcash_serialize_to_vec().unwrap();

    assert!(
        &new_bytes != REGTEST_POS_BLOCK_BYTES[0],
        "test invalidated if the serialization has not been changed"
    );

    tf.push_instr_load_pos_bytes(&new_bytes, SHOULD_FAIL);
    tf.push_instr_expect_pos_chain_length(0, 0);

    test_bytes(tf.write_to_bytes());
}

#[test]
fn crosslink_test_basic_finality() {
    set_test_name(function_name!());
    let mut tf = TF::new(&PROTOTYPE_PARAMETERS);

    let hashes = regtest_block_hashes();

    for i2 in 0..REGTEST_BLOCK_BYTES.len() {
        tf.push_instr_expect_pow_block_finality(&hashes[i2], None, 0);
    }

    // let hashes = Vec::with_capacity(REGTEST_BLOCK_BYTES.len());
    for i in 0..REGTEST_BLOCK_BYTES.len() {
        // hashes.push()
        tf.push_instr_load_pow_bytes(REGTEST_BLOCK_BYTES[i], 0);

        for i2 in 0..REGTEST_BLOCK_BYTES.len() {
            let finality = if i2 > i {
                None
            } else if i2 == 3 && i == 3 {
                Some(TFLBlockFinality::NotYetFinalized)
            } else if i2 == 2 && i > 3 {
                Some(TFLBlockFinality::NotYetFinalized)
            } else {
                Some(TFLBlockFinality::NotYetFinalized)
            };
            tf.push_instr_expect_pow_block_finality(&hashes[i2], finality, 0);
        }
    }

    for i in 0..REGTEST_POS_BLOCK_BYTES.len() {
        tf.push_instr_load_pos_bytes(REGTEST_POS_BLOCK_BYTES[i], 0);

        for i2 in 0..REGTEST_BLOCK_BYTES.len() {
            let finality = if i2 == 2 {
                // unpicked sidechain
                if i < 1 {
                    Some(TFLBlockFinality::NotYetFinalized)
                } else {
                    // Some(TFLBlockFinality::CantBeFinalized)
                    None
                }
            } else if i2 <= REGTEST_POW_IDX_FINALIZED_BY_POS_BLOCK[i] {
                Some(TFLBlockFinality::Finalized)
            } else {
                Some(TFLBlockFinality::NotYetFinalized)
            };
            tf.push_instr_expect_pow_block_finality(&hashes[i2], finality, 0);
        }
    }

    test_bytes(tf.write_to_bytes());
}

#[ignore]
#[test]
fn reject_pos_block_with_lt_sigma_headers() {
    set_test_name(function_name!());
    let mut tf = TF::new(&PROTOTYPE_PARAMETERS);

    for i in 0..4 {
        tf.push_instr_load_pow_bytes(REGTEST_BLOCK_BYTES[i], 0);
    }

    let mut bft_block_and_fat_ptr =
        BftBlockAndFatPointerToIt::zcash_deserialize(REGTEST_POS_BLOCK_BYTES[0]).unwrap();
    bft_block_and_fat_ptr
        .block
        .headers
        .truncate(bft_block_and_fat_ptr.block.headers.len() - 1);
    let new_bytes = bft_block_and_fat_ptr.zcash_serialize_to_vec().unwrap();
    assert!(
        &new_bytes != REGTEST_POS_BLOCK_BYTES[0],
        "test invalidated if the serialization has not been changed"
    );

    tf.push_instr_load_pos_bytes(&new_bytes, 0);
    tf.push_instr_expect_pos_chain_length(0, 0);
}

#[test]
fn crosslink_test_pow_to_pos_link() {
    set_test_name(function_name!());
    let mut tf = TF::new(&PROTOTYPE_PARAMETERS);

    tf.push_instr_load_pow_bytes(REGTEST_BLOCK_BYTES[0], 0);
    tf.push_instr_load_pow_bytes(REGTEST_BLOCK_BYTES[1], 0);
    tf.push_instr_load_pow_bytes(REGTEST_BLOCK_BYTES[2], 0);
    tf.push_instr_load_pow_bytes(REGTEST_BLOCK_BYTES[3], 0);
    tf.push_instr_load_pow_bytes(REGTEST_BLOCK_BYTES[4], 0);

    // TODO: maybe have push_instr return data?
    tf.push_instr_load_pos_bytes(REGTEST_POS_BLOCK_BYTES[0], 0);
    let pos_0 = BftBlockAndFatPointerToIt::zcash_deserialize(REGTEST_POS_BLOCK_BYTES[0]).unwrap();
    let pos_0_fat_ptr = zebra_chain::block::FatPointerToBftBlock {
        vote_for_block_without_finalizer_public_key: pos_0
            .fat_ptr
            .vote_for_block_without_finalizer_public_key,
        signatures: pos_0
            .fat_ptr
            .signatures
            .iter()
            .map(|sig| zebra_chain::block::FatPointerSignature {
                public_key: sig.public_key,
                vote_signature: sig.vote_signature,
            })
            .collect(),
    };

    let mut pow_5 = Block::zcash_deserialize(REGTEST_BLOCK_BYTES[5]).unwrap();
    pow_5.header = Arc::new(BlockHeader {
        version: 5,
        fat_pointer_to_bft_block: pos_0_fat_ptr.clone(),
        ..*pow_5.header
    });
    // let pow_5_hash = pow_5.hash();
    tf.push_instr_load_pow(&pow_5, 0);

    // let mut pow_6 = Block::zcash_deserialize(REGTEST_BLOCK_BYTES[6]).unwrap();
    // pow_6.header = Arc::new(BlockHeader {
    //     version: 5,
    //     fat_pointer_to_bft_block: pos_0_fat_ptr.clone(),
    //     previous_block_hash: pow_5_hash,
    //     ..*pow_6.header
    // });
    // // let pow5_hash = pow_5.hash();
    // tf.push_instr_load_pow(&pow_6, 0);

    // tf.write_to_file(&Path::new(&format!("{}.zeccltf", function_name!())));
    test_bytes(tf.write_to_bytes());
}

#[test]
fn crosslink_reject_pow_chain_fork_that_is_competing_against_a_shorter_finalized_pow_chain() {
    set_test_name(function_name!());
    test_path(PathBuf::from(
        "../crosslink-test-data/wrong_branch_test1_short_pos_long.zeccltf",
    ));
}

#[test]
fn crosslink_pow_switch_to_finalized_chain_fork_even_though_longer_chain_exists() {
    set_test_name(function_name!());
    test_path(PathBuf::from(
        "../crosslink-test-data/wrong_branch_test2_long_short_pos.zeccltf",
    ));
}

// NOTE: this is very similar to the RPC get_block_template code
#[derive(Clone, Debug)]
struct BlockGen {
    network: Network,
    // NOTE: these roots need updating if we include shielded transactions
    sapling_root: sapling::tree::Root,
    orchard_root: orchard::tree::Root,

    history_tree: HistoryTree,

    tip: Arc<Block>,
}

impl BlockGen {
    const REGTEST_GENESIS_HASH: BlockHash = BlockHash([
        0x27, 0xe3, 0x01, 0x34, 0xd6, 0x20, 0xe9, 0xfe, 0x61, 0xf7, 0x19, 0x93, 0x83, 0x20, 0xba,
        0xb6, 0x3e, 0x7e, 0x72, 0xc9, 0x1b, 0x5e, 0x23, 0x02, 0x56, 0x76, 0xf9, 0x0e, 0xd8, 0x11,
        0x9f, 0x02,
    ]);

    #[allow(dead_code)]
    pub fn init_regtest_at_tip(tip: Arc<Block>) -> Self {
        BlockGen {
            network: Network::new_regtest(Default::default()),
            sapling_root: sapling::tree::NoteCommitmentTree::default().root(),
            orchard_root: orchard::tree::NoteCommitmentTree::default().root(),
            history_tree: HistoryTree::default(),
            tip,
        }
    }

    pub fn init_at_genesis_plus_1(
        network: Network,
        genesis_hash: BlockHash,
        miner_address: &Address,
    ) -> Self {
        let history_tree = HistoryTree::default();
        let time = chrono::DateTime::<Utc>::from_timestamp(1758127904, 0).expect("valid time");

        // NOTE: the first block after genesis appears to need this specific difficulty
        let difficulty_threshold =
            CompactDifficulty::from_bytes_in_display_order(&[0x20, 0x0c, 0xa6, 0x3f]).unwrap();

        let tip = BlockGen::create_block(
            &network,
            miner_address,
            BlockHeight(1),
            genesis_hash,
            time,
            &history_tree,
            difficulty_threshold,
        );
        BlockGen {
            network,
            history_tree,
            sapling_root: sapling::tree::NoteCommitmentTree::default().root(),
            orchard_root: orchard::tree::NoteCommitmentTree::default().root(),
            tip,
        }
    }

    pub fn create_block(
        network: &Network,
        miner_address: &Address,
        height: BlockHeight,
        previous_block_hash: BlockHash,
        time: DateTime<Utc>,
        history_tree: &HistoryTree,
        difficulty_threshold: CompactDifficulty,
    ) -> Arc<zebra_chain::block::Block> {
        let coinbase_outputs =
            zebra_rpc::methods::types::get_block_template::standard_coinbase_outputs(
                network,
                height,
                miner_address,
                zebra_chain::amount::Amount::new(0), // TODO: update if we want to sim transactions
            );

        let coinbase_tx = if true {
            zebra_chain::transaction::Transaction::new_v4_coinbase(
                height,
                coinbase_outputs,
                Vec::new(),
            )
        } else {
            zebra_chain::transaction::Transaction::new_v5_coinbase(
                network,
                height,
                coinbase_outputs,
                Vec::new(),
            )
        };
        let default_roots =
            zebra_rpc::methods::types::get_block_template::calculate_default_root_hashes(
                &coinbase_tx.clone().into(),
                &[],
                [0; 32].into(),
            );

        Arc::new(Block {
            header: Arc::new(BlockHeader {
                version: 6,
                previous_block_hash,
                merkle_root: default_roots.merkle_root(),
                commitment_bytes: HexDebug(<[u8; 32]>::from(
                    history_tree.hash().unwrap_or([0u8; 32].into()),
                )),
                time,
                difficulty_threshold,
                nonce: HexDebug([0; 32]),
                solution: work::equihash::Solution::Regtest([0; 36]),
                fat_pointer_to_bft_block: FatPointerToBftBlock::null(),
            }),

            transactions: vec![coinbase_tx.into()],
        })
    }

    pub fn next_block(&mut self, miner_address: &Address) -> Arc<zebra_chain::block::Block> {
        // NOTE: it's not completely obvious where this should be done. Having it here allows for
        // tip modification by the user, but means that the history_tree is never visibly
        // up-to-date.
        self.history_tree
            .push(
                &self.network,
                self.tip.clone(),
                &self.sapling_root,
                &self.orchard_root,
            )
            .unwrap();

        let height = zebra_chain::block::Height(self.tip.coinbase_height().unwrap().0 + 1);
        let hash = self.tip.header.hash();
        let time = self.tip.header.time + Duration::from_secs(70);

        let difficulty_threshold =
            CompactDifficulty::from_bytes_in_display_order(&[0x20, 0x0f, 0x0f, 0x0f]).unwrap();
        self.tip = BlockGen::create_block(
            &self.network,
            miner_address,
            height,
            hash,
            time,
            &self.history_tree,
            difficulty_threshold,
        );

        self.tip.clone()
    }
}

#[test]
fn crosslink_gen_pow_fork() {
    set_test_name(function_name!());
    let mut tf = TF::new(&PROTOTYPE_PARAMETERS);

    let network = Network::new_regtest(Default::default());
    let miner_address = Address::decode(&network, "t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v").unwrap();
    let mut gen =
        BlockGen::init_at_genesis_plus_1(network, BlockGen::REGTEST_GENESIS_HASH, &miner_address);
    tf.push_instr_load_pow(&gen.tip, 0);

    for _ in 2..4 {
        tf.push_instr_load_pow(&gen.next_block(&miner_address), 0);
    }
    let mut genb = gen.clone();

    for _ in 4..7 {
        tf.push_instr_load_pow(&gen.next_block(&miner_address), 0);
    }
    tf.push_instr_expect_pow_chain_length(7, 0);

    let miner_address2 = zcash_keys::address::Address::Tex([1; 20]);
    for _ in 4..8 {
        tf.push_instr_load_pow(&genb.next_block(&miner_address2), 0);
    }
    tf.push_instr_expect_pow_chain_length(8, 0);

    // Result:
    // P  LOAD_POW (1 - f2245bd187293755539ac981d156e0e09a5d5f3e985abae974c71f04d953a2f1, parent: 029f11d80ef9765602235e1bc9727e3eb6ba20839319f761fee920d63401e327)
    // P  LOAD_POW (2 - e01789fab97edbdd127a0162348b1882200ba26043f392bce495362e7c20c354, parent: f2245bd187293755539ac981d156e0e09a5d5f3e985abae974c71f04d953a2f1)
    // P  LOAD_POW (3 - c19351b6534ce047a7da8d3f60f04383206c9f9cd9f6edd51e39c15f32b767ef, parent: e01789fab97edbdd127a0162348b1882200ba26043f392bce495362e7c20c354)
    // P  LOAD_POW (4 - f8214b23b5539cbeb93917d91dd85e7209824429f5aa3905baf273ae7a2d9586, parent: c19351b6534ce047a7da8d3f60f04383206c9f9cd9f6edd51e39c15f32b767ef)
    // P  LOAD_POW (5 - ee3f15e63275d68320e998c148cde1f50c6cb7f61c308cbe65b2fb8bf373b088, parent: f8214b23b5539cbeb93917d91dd85e7209824429f5aa3905baf273ae7a2d9586)
    // P  LOAD_POW (6 - 5360f9cf81a7d40cd4d305efc1a4127ba5fe0e3c7cd3fe9cea952c1186f67709, parent: ee3f15e63275d68320e998c148cde1f50c6cb7f61c308cbe65b2fb8bf373b088)
    // P  EXPECT_POW_CHAIN_LENGTH (7)
    // P  LOAD_POW (4 - 669391145fdbadc09b622657aec6166927efda199b20662e66e5154b5d35ee95, parent: c19351b6534ce047a7da8d3f60f04383206c9f9cd9f6edd51e39c15f32b767ef)
    // P  LOAD_POW (5 - 8fde5bb6d9d7b21795b06da74b574167628fce2cf5f651ddc41d9d33dc76ca8e, parent: 669391145fdbadc09b622657aec6166927efda199b20662e66e5154b5d35ee95)
    // P  LOAD_POW (6 - bbb0e46c50d3730cd217c073b3da08929d33ec8cd5800b5226027c7a48ba016c, parent: 8fde5bb6d9d7b21795b06da74b574167628fce2cf5f651ddc41d9d33dc76ca8e)
    // P  LOAD_POW (7 - 879e7c77dd9c141689ff4f05bb0b80e153639062b5371f2ffced3acaaf563d31, parent: bbb0e46c50d3730cd217c073b3da08929d33ec8cd5800b5226027c7a48ba016c)
    // P  EXPECT_POW_CHAIN_LENGTH (8)

    test_bytes(tf.write_to_bytes());
}

fn create_pos_and_ptr_to_finalize_pow(
    bft_height: u32,
    pow_blocks: &[Arc<Block>],
) -> BftBlockAndFatPointerToIt {
    assert_eq!(
        pow_blocks.len(),
        PROTOTYPE_PARAMETERS.bc_confirmation_depth_sigma as usize
    );

    let block = BftBlock::try_from(
        &PROTOTYPE_PARAMETERS,
        bft_height,
        FatPointerToBftBlock::null(),
        pow_blocks[0].coinbase_height().expect("valid height").0,
        vec![
            pow_blocks[0].header.as_ref().clone(),
            pow_blocks[1].header.as_ref().clone(),
            pow_blocks[2].header.as_ref().clone(),
        ],
    )
    .expect("valid PoS block");

    BftBlockAndFatPointerToIt {
        block,
        fat_ptr: FatPointerToBftBlock::null(), // TODO
    }
}

#[ignore]
#[test]
fn crosslink_gen_pow_and_no_signature_no_roster_pos() {
    set_test_name(function_name!());
    let mut tf = TF::new(&PROTOTYPE_PARAMETERS);

    let network = Network::new_regtest(Default::default());
    let miner_address = Address::decode(&network, "t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v").unwrap();
    let mut gen =
        BlockGen::init_at_genesis_plus_1(network, BlockGen::REGTEST_GENESIS_HASH, &miner_address);

    let mut pow_common = vec![gen.tip.clone()];
    for _ in 2..4 {
        pow_common.push(gen.next_block(&miner_address));
    }
    for block in &pow_common {
        tf.push_instr_load_pow(block, 0);
    }

    let bft = create_pos_and_ptr_to_finalize_pow(1, &pow_common[0..3]);
    tf.push_instr_load_pos(&bft, 0);

    for _ in 4..7 {
        tf.push_instr_load_pow(&gen.next_block(&miner_address), 0);
    }
    tf.push_instr_expect_pow_chain_length(7, 0);

    test_bytes(tf.write_to_bytes());
}

#[test]
fn crosslink_force_roster() {
    set_test_name(function_name!());
    let mut tf = TF::new(&PROTOTYPE_PARAMETERS);

    tf.push_instr_expect_roster_includes([0xab; 32], 42, SHOULD_FAIL);

    tf.push_instr_roster_force_include([0xab; 32], 42, 0);

    tf.push_instr_expect_roster_includes([0xab; 32], 42, 0);
    tf.push_instr_expect_roster_includes([0xab; 32], TEST_STAKE_IGNORED, 0);
    tf.push_instr_expect_roster_includes([0xab; 32], 43, SHOULD_FAIL);
    tf.push_instr_expect_roster_includes([0xba; 32], 42, SHOULD_FAIL);

    test_bytes(tf.write_to_bytes());
}

/*
#[test]
fn crosslink_add_newcomer_to_roster_via_pow() {
    set_test_name(function_name!());
    let mut tf = TF::new(&PROTOTYPE_PARAMETERS);

    // let (_, prv_key, pub_key) = rng_private_public_key_from_address(&[0]);

    // tf.push_instr_roster_force_include(pub_key, 42000, 0);
    // tf.push_instr_expect_roster_includes(pub_key, 42000, SHOULD_FAIL);

    let network = Network::new_regtest(Default::default());
    let miner_address = Address::decode(&network, "t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v").unwrap();
    let mut gen =
        BlockGen::init_at_genesis_plus_1(network, BlockGen::REGTEST_GENESIS_HASH, &miner_address);

    gen.tip = Arc::new(Block {
        header: Arc::new(BlockHeader {
            temp_command_buf: CommandBuf::from_str("ADD|1234|some_pub_key"),
            ..gen.tip.header.as_ref().clone()
        }),
        ..gen.tip.as_ref().clone()
    });

    let mut pow_common = vec![gen.tip.clone()];
    for _ in 2..4 {
        pow_common.push(gen.next_block(&miner_address));
    }
    for block in &pow_common[0..3] {
        tf.push_instr_load_pow(block, 0);
    }

    let bft = create_pos_and_ptr_to_finalize_pow(1, &pow_common[0..3]);
    tf.push_instr_load_pos(&bft, 0);

    let (_, _prv_key, pub_key) =
        zebrad::components::crosslink::rng_private_public_key_from_address("some_pub_key".as_bytes());
    tf.push_instr_expect_roster_includes(pub_key.into(), 1234, 0);

    test_bytes(tf.write_to_bytes());
}
*/

// TODO:
// - reject signatures from outside the roster
// - reject pos block with < 2/3rds roster stake
// - reject pos block with signatures from the previous, but not current roster
// > require correctly-signed incorrect data:
//   - reject pos block with > sigma headers
//   - reject pos block with < sigma headers
//   - reject pos block where headers don't form subchain (hdrs[i].hash() != hdrs[i+1].previous_block_hash)
//   - repeat all signature tests but for the *next* pos block's fat pointer back
// - reject pos block that does have the correct fat pointer *hash* to prev block
// ...

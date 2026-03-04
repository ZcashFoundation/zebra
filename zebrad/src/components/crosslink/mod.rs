//! Internal Zebra service for managing the Crosslink consensus protocol.

#![allow(clippy::print_stdout)]
#![allow(unexpected_cfgs, unused, missing_docs)]

use zcash_primitives::transaction::{StakingAction, StakingActionKind};
use zebra_chain::block::FatPointerToBftBlock;
use zebra_chain::crosslink::*;
use zebra_chain::serialization::{
    SerializationError, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize,
};
use zebra_state::crosslink::*;

use rand::{Rng, SeedableRng};
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hasher};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::time::Instant;
use tracing::{error, info, warn};

use ed25519_zebra::SigningKey as MalPrivateKey;
use ed25519_zebra::VerificationKeyBytes as MalPublicKey;

pub mod config;
pub mod service;
pub mod test_format;

use service::{TFLServiceCalls, TFLServiceHandle};

use zebra_chain::block::{
    Block, CountedHeader, Hash as BlockHash, Header as BlockHeader, Height as BlockHeight,
};
use zebra_node_services::mempool::{Request as MempoolRequest, Response as MempoolResponse};
use zebra_state::{Request as StateRequest, Response as StateResponse};

use std::sync::Mutex;
use tokio::sync::Mutex as TokioMutex;

pub static TEST_INSTR_C: Mutex<usize> = Mutex::new(0);
pub static TEST_MODE: Mutex<bool> = Mutex::new(false);
pub static TEST_FAILED: Mutex<i32> = Mutex::new(0);
pub static TEST_FAILED_INSTR_IDXS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
pub static TEST_CHECK_ASSERT: Mutex<bool> = Mutex::new(false);
pub static TEST_INSTR_PATH: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);
pub static TEST_INSTR_BYTES: Mutex<Vec<u8>> = Mutex::new(Vec::new());
pub static TEST_INSTRS: Mutex<Vec<test_format::TFInstr>> = Mutex::new(Vec::new());
pub static TEST_SHUTDOWN_FN: Mutex<fn()> = Mutex::new(|| ());
pub static TEST_PARAMS: Mutex<Option<ZcashCrosslinkParameters>> = Mutex::new(None);
pub static TEST_NAME: Mutex<&'static str> = Mutex::new("‰‰TEST_NAME_NOT_SET‰‰");

pub fn dump_test_instrs() {
    #![allow(clippy::print_stderr)]

    let failed_instr_idxs_lock = TEST_FAILED_INSTR_IDXS.lock();
    let failed_instr_idxs = failed_instr_idxs_lock.as_ref().unwrap();
    if failed_instr_idxs.is_empty() {
        eprintln!(
            "no failed instructions recorded. We should have at least 1 failed instruction here"
        );
    }

    let done_instr_c = *TEST_INSTR_C.lock().unwrap();

    let mut failed_instr_idx_i = 0;
    let instrs_lock = TEST_INSTRS.lock().unwrap();
    let instrs: &Vec<test_format::TFInstr> = instrs_lock.as_ref();
    let bytes_lock = TEST_INSTR_BYTES.lock().unwrap();
    let bytes = bytes_lock.as_ref();
    for instr_i in 0..instrs.len() {
        let col = if failed_instr_idx_i < failed_instr_idxs.len()
            && instr_i == failed_instr_idxs[failed_instr_idx_i]
        {
            failed_instr_idx_i += 1;
            "\x1b[91m F  " // red
        } else if instr_i < done_instr_c {
            "\x1b[92m P  " // green
        } else {
            "\x1b[37m    " // grey
        };
        eprintln!(
            "  {}{}\x1b[0;0m",
            col,
            &test_format::TFInstr::string_from_instr(bytes, &instrs[instr_i])
        );
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct TFLServiceInternal {
    my_public_key: MalPublicKey,
    latest_final_block: Option<(BlockHeight, BlockHash)>,
    tfl_is_activated: bool,

    // channels
    final_change_tx: broadcast::Sender<(BlockHeight, BlockHash)>,

    bft_msg_flags: u64,
    bft_err_flags: u64,
    bft_blocks: Vec<BftBlock>,
    fat_pointer_to_tip: FatPointerToBftBlock,
    our_set_bft_string: Option<String>,
    active_bft_string: Option<String>,

    validators_keys_to_names: HashMap<MalPublicKey, String>,
    validators_at_current_height: Vec<BftValidator>,

    current_bc_final: Option<(BlockHeight, BlockHash)>,
}

async fn block_height_from_hash(call: &TFLServiceCalls, hash: BlockHash) -> Option<BlockHeight> {
    if let Ok(StateResponse::KnownBlock(Some(known_block))) =
        (call.state)(StateRequest::KnownBlock(hash.into())).await
    {
        Some(known_block.height)
    } else {
        None
    }
}

async fn block_height_hash_from_hash(
    call: &TFLServiceCalls,
    hash: BlockHash,
) -> Option<(BlockHeight, BlockHash)> {
    if let Ok(StateResponse::BlockHeader {
        height,
        hash: check_hash,
        ..
    }) = (call.state)(StateRequest::BlockHeader(hash.into())).await
    {
        assert_eq!(hash, check_hash);
        Some((height, hash))
    } else {
        None
    }
}

async fn tfl_reorg_final_block_height_hash(
    call: &TFLServiceCalls,
) -> Option<(BlockHeight, BlockHash)> {
    let locator = (call.state)(StateRequest::BlockLocator).await;

    if let Ok(StateResponse::BlockLocator(hashes)) = locator {
        let result_1 = match hashes.last() {
            Some(hash) => block_height_from_hash(call, *hash)
                .await
                .map(|height| (height, *hash)),
            None => None,
        };

        result_1
    } else {
        None
    }
}

async fn tfl_final_block_height_hash(
    internal_handle: &TFLServiceHandle,
) -> Option<(BlockHeight, BlockHash)> {
    let mut internal = internal_handle.internal.lock().await;
    tfl_final_block_height_hash_pre_locked(internal_handle, &mut internal).await
}

async fn tfl_final_block_height_hash_pre_locked(
    internal_handle: &TFLServiceHandle,
    internal: &mut TFLServiceInternal,
) -> Option<(BlockHeight, BlockHash)> {
    if internal.latest_final_block.is_some() {
        internal.latest_final_block
    } else {
        tfl_reorg_final_block_height_hash(&internal_handle.call).await
    }
}

pub fn rng_private_public_key_from_address(
    addr: &[u8],
) -> (rand::rngs::StdRng, MalPrivateKey, MalPublicKey) {
    let mut hasher = DefaultHasher::new();
    hasher.write(addr);
    let seed = hasher.finish();
    let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
    let private_key = MalPrivateKey::new(&mut rng);
    let public_key = (&private_key).into();
    (rng, private_key, public_key)
}

async fn propose_new_bft_block(tfl_handle: &TFLServiceHandle) -> Option<BftBlock> {
    let call = tfl_handle.call.clone();
    let params = &PROTOTYPE_PARAMETERS;
    let (tip_height, tip_hash) =
        if let Ok(StateResponse::Tip(val)) = (call.state)(StateRequest::Tip).await {
            if val.is_none() {
                return None;
            }
            val.unwrap()
        } else {
            return None;
        };

    use std::ops::Sub;
    use zebra_chain::block::HeightDiff as BlockHeightDiff;

    let finality_candidate_height = tip_height.sub(BlockHeightDiff::from(
        params.bc_confirmation_depth_sigma as i64,
    ));

    let finality_candidate_height = if let Some(h) = finality_candidate_height {
        h
    } else {
        info!(
            "not enough blocks to enforce finality; tip height: {}",
            tip_height.0
        );
        return None;
    };

    let (latest_final_block, latest_bft_block_hash) = {
        let internal = tfl_handle.internal.lock().await;
        (
            internal.latest_final_block,
            internal
                .bft_blocks
                .last()
                .map_or(Blake3Hash([0u8; 32]), |b| b.blake3_hash()),
        )
    };
    let is_improved_final =
        latest_final_block.is_none() || finality_candidate_height > latest_final_block.unwrap().0;

    if !is_improved_final {
        info!(
            "candidate block can't be final: height {}, final height: {:?}",
            finality_candidate_height.0, latest_final_block
        );
        return None;
    }

    let resp = (call.state)(StateRequest::BlockHeader(finality_candidate_height.into())).await;

    let candidate_hash = if let Ok(StateResponse::BlockHeader { hash, .. }) = resp {
        hash
    } else {
        panic!("TODO: improve error handling.");
    };

    let resp = (call.state)(StateRequest::FindBlockHeaders {
        known_blocks: vec![candidate_hash],
        stop: None,
    })
    .await;

    let headers: Vec<BlockHeader> = if let Ok(StateResponse::BlockHeaders(hdrs)) = resp {
        hdrs.into_iter()
            .map(|ch| Arc::unwrap_or_clone(ch.header))
            .collect()
    } else {
        panic!("TODO: improve error handling.");
    };

    let mut internal = tfl_handle.internal.lock().await;

    match BftBlock::try_from(
        params,
        internal.bft_blocks.len() as u32 + 1,
        internal.fat_pointer_to_tip.clone(),
        0,
        headers,
    ) {
        Ok(v) => Some(v),
        Err(e) => {
            warn!("Unable to create BftBlock to propose, Error={:?}", e,);
            None
        }
    }
}

pub(crate) async fn validate_bft_block(
    tfl_handle: &TFLServiceHandle,
    new_block: &BftBlock,
) -> BftValidationStatus {
    let mut internal = tfl_handle.internal.lock().await;
    validate_bft_block_already_locked(tfl_handle, &mut internal, new_block).await
}

async fn validate_bft_block_already_locked(
    tfl_handle: &TFLServiceHandle,
    internal: &mut TFLServiceInternal,
    new_block: &BftBlock,
) -> BftValidationStatus {
    let call = tfl_handle.call.clone();
    let params = &PROTOTYPE_PARAMETERS;

    let prev_hash = new_block.previous_block_fat_ptr.points_at_blake3_hash();
    let expected_hash = internal
        .bft_blocks
        .last()
        .map_or(Blake3Hash([0u8; 32]), |b| b.blake3_hash());
    if prev_hash != expected_hash {
        error!("validate_bft_block: prev_hash {prev_hash} != expected {expected_hash}, bft_blocks.len()={}", internal.bft_blocks.len());
        return BftValidationStatus::Fail;
    }

    let final_hash = new_block.headers.first().expect("at least 1 header").hash();
    if let Ok(StateResponse::KnownBlock(Some(_))) =
        (call.state)(StateRequest::KnownBlock(final_hash.into())).await
    {
        // block exists in state
    } else {
        warn!("validate_bft_block: block hash {final_hash} not found in state (Indeterminate)");
        return BftValidationStatus::Indeterminate;
    }
    BftValidationStatus::Pass
}

pub(crate) async fn process_decided_bft_block(
    tfl_handle: &TFLServiceHandle,
    new_block: &BftBlock,
    fat_pointer: &FatPointerToBftBlock,
) -> Vec<(MalPublicKey, u64)> {
    let call = tfl_handle.call.clone();
    let params = &PROTOTYPE_PARAMETERS;

    let mut internal = tfl_handle.internal.lock().await;

    if fat_pointer.points_at_blake3_hash() != new_block.blake3_hash() {
        error!(
            "Fat Pointer hash does not match block hash. fp: {} block: {}",
            fat_pointer.points_at_blake3_hash(),
            new_block.blake3_hash()
        );
        panic!();
    }
    if !fat_pointer.validate_signatures() {
        error!("Signatures are not valid. Rejecting block.");
        panic!();
    }

    assert_eq!(
        validate_bft_block_already_locked(tfl_handle, &mut internal, new_block).await,
        BftValidationStatus::Pass
    );

    let new_final_hash = new_block.headers.first().expect("at least 1 header").hash();
    let new_final_height = block_height_from_hash(&call, new_final_hash).await.unwrap();
    let insert_i = new_block.height as usize - 1;

    // Ensure there are enough blocks to overwrite this at the correct index
    for i in internal.bft_blocks.len()..=insert_i {
        internal.bft_blocks.push(BftBlock {
            version: 0,
            height: i as u32,
            previous_block_fat_ptr: FatPointerToBftBlock::null(),
            finalization_candidate_height: 0,
            headers: Vec::new(),
        });
    }

    if insert_i > 0 {
        assert_eq!(
            internal.bft_blocks[insert_i - 1].blake3_hash(),
            new_block.previous_block_fat_ptr.points_at_blake3_hash()
        );
    }
    assert!(insert_i == 0 || new_block.previous_block_hash() != Blake3Hash([0u8; 32]));
    assert!(
        internal.bft_blocks[insert_i].headers.is_empty(),
        "{:?}",
        internal.bft_blocks[insert_i]
    );
    assert!(!new_block.headers.is_empty());
    internal.bft_blocks[insert_i] = new_block.clone();
    internal.fat_pointer_to_tip = fat_pointer.clone();
    internal.latest_final_block = Some((new_final_height, new_final_hash));

    match (call.state)(zebra_state::Request::CrosslinkFinalizeBlock(new_final_hash)).await {
        Ok(zebra_state::Response::CrosslinkFinalized(hash)) => {
            info!("Successfully crosslink-finalized {}", hash);
            assert_eq!(
                hash, new_final_hash,
                "PoW finalized hash should now match ours"
            );
        }
        Ok(_) => unreachable!("wrong response type"),
        Err(err) => {
            error!(?err);
        }
    }

    {
        let new_bc_final = internal.latest_final_block;

        if let Some(new_final_height_hash) = new_bc_final {
            let start_hash = if let Some(prev_height_hash) = internal.current_bc_final {
                prev_height_hash.1
            } else {
                new_final_height_hash.1
            };

            let (new_final_height_hashes, new_final_blocks) = tfl_block_sequence(
                &call,
                start_hash,
                Some(new_final_height_hash),
                true,
                true,
            )
            .await;

            let pos_total_reward: u64 = 6000;

            for i in 0..new_final_height_hashes.len() {
                let new_final_height_hash = &new_final_height_hashes[i];
                if let Some((_, prev_hash)) = internal.current_bc_final {
                    if prev_hash == new_final_height_hash.1 {
                        continue;
                    }
                }

                // Divide the reward between finalizers
                {
                    let finalizers = &mut internal.validators_at_current_height;
                    let mut total_voting_power = 0;
                    let mut max_power_finalizer_i: Option<usize> = None;

                    for finalizer_i in 0..finalizers.len() {
                        let finalizer = &finalizers[finalizer_i];
                        if finalizer.voting_power == 0 {
                            continue;
                        }

                        if max_power_finalizer_i.is_none()
                            || finalizers[max_power_finalizer_i.unwrap()].voting_power
                                < finalizer.voting_power
                        {
                            max_power_finalizer_i = Some(finalizer_i);
                        }

                        total_voting_power += finalizer.voting_power;
                    }

                    let mut sum_reward = 0_u64;
                    for finalizer_i in 0..finalizers.len() {
                        let finalizer = &mut finalizers[finalizer_i];
                        if finalizer.voting_power == 0
                            || finalizer_i
                                == max_power_finalizer_i
                                    .expect("there must be a max finalizer if at least 1 is non-0")
                        {
                            continue;
                        }

                        let mul: u128 =
                            (finalizer.voting_power as u128) * (pos_total_reward as u128);
                        let reward = (mul / (total_voting_power as u128)) as u64;
                        sum_reward += reward;
                        finalizer.voting_power += reward;
                    }

                    if let Some(finalizer_i) = max_power_finalizer_i {
                        let finalizer = &mut finalizers[finalizer_i];
                        let mul: u128 =
                            (finalizer.voting_power as u128) * (pos_total_reward as u128);
                        let reward = (mul / (total_voting_power as u128)) as u64;
                        let rem_reward = pos_total_reward - sum_reward;
                        assert!(
                            reward <= rem_reward,
                            "should be at least the expected share remaining.\n\
                            expected reward: {}, remaining reward: {}",
                            reward,
                            rem_reward
                        );

                        if reward != rem_reward {
                            info!(
                                "Max finalizer given rounding error: expected {}, got {}, bonus: {}",
                                reward,
                                rem_reward,
                                rem_reward - reward
                            );
                        }

                        finalizer.voting_power += rem_reward;
                    }
                }

                // Modify the stake for members
                if let Some(new_final_block) = &new_final_blocks[i] {
                    let cmd_c = update_roster_for_block(&mut internal, new_final_block);
                    if cmd_c > 0 {
                        info!(
                            "Applied {} commands to roster from PoW height {}",
                            cmd_c, new_final_height_hashes[i].0 .0,
                        );
                    }
                } else {
                    error!(
                        "failed to get known block at {:?}",
                        new_final_height_hashes[i]
                    );
                    debug_assert!(false, "this shouldn't happen");
                }

                let _ = internal.final_change_tx.send(*new_final_height_hash);
            }
        }
        internal.current_bc_final = new_bc_final;
    }

    // Return roster as (public_key, voting_power) pairs
    internal
        .validators_at_current_height
        .iter()
        .map(|v| (v.public_key, v.voting_power))
        .collect()
}

fn update_roster_for_cmd(
    roster: &mut Vec<BftValidator>,
    validators_keys_to_names: &mut HashMap<MalPublicKey, String>,
    action: &StakingAction,
) -> usize {
    let (has_add, sub_key_name, is_clear) = match action.kind {
        StakingActionKind::Add => (true, None, false),
        StakingActionKind::Sub => (
            false,
            Some((action.target, &action.insecure_target_name)),
            false,
        ),
        StakingActionKind::Clear => (
            false,
            Some((action.target, &action.insecure_target_name)),
            true,
        ),
        StakingActionKind::Move => (
            true,
            Some((action.source, &action.insecure_source_name)),
            false,
        ),
        StakingActionKind::MoveClear => (
            true,
            Some((action.source, &action.insecure_source_name)),
            true,
        ),
    };

    let mut amount = action.val;
    if let Some((sub_key, sub_name)) = sub_key_name {
        let sub_key = BftValidatorAddress(sub_key.into());
        let Some(member) = roster.iter_mut().find(|cmp| cmp.public_key == sub_key.0) else {
            warn!(
                "Roster command invalid: can't subtract from non-present finalizer \"{}\"",
                sub_name
            );
            return 0;
        };

        if member.voting_power < action.val {
            if is_clear {
                warn!("Roster command invalid: can't clear the finalizer to a higher current value \"{}\"/{}: {} => {}",
                    sub_name, sub_key, member.voting_power, action.val);
            } else {
                warn!("Roster command invalid: can't subtract more from the finalizer than their current value \"{}\"/{}: {} - {}",
                    sub_name, sub_key, member.voting_power, action.val);
            }
            return 0;
        }

        if is_clear {
            amount = member.voting_power - action.val
        };

        member.voting_power -= amount;
    }

    if has_add {
        let add_key = BftValidatorAddress(action.target.into());
        if let Some(member) = roster.iter_mut().find(|cmp| cmp.public_key == add_key.0) {
            member.voting_power += amount;
        } else {
            roster.push(BftValidator::new(add_key.0, amount));
            validators_keys_to_names.insert(add_key.0, action.insecure_target_name.clone());
        }
    }

    1
}

fn update_roster_for_block(internal: &mut TFLServiceInternal, block: &Block) -> usize {
    let roster = &mut internal.validators_at_current_height;
    let validators_keys_to_names = &mut internal.validators_keys_to_names;
    let mut cmd_c = 0;

    for tx in &block.transactions {
        if let zebra_chain::transaction::Transaction::VCrosslink { staking_action, .. } =
            tx.as_ref()
        {
            if let Some(staking_action) = staking_action {
                cmd_c += update_roster_for_cmd(roster, validators_keys_to_names, staking_action);
            }
        };
    }

    cmd_c
}

const MAIN_LOOP_SLEEP_INTERVAL: Duration = Duration::from_millis(125);
const MAIN_LOOP_INFO_DUMP_INTERVAL: Duration = Duration::from_millis(8000);

pub fn run_tfl_test(internal_handle: TFLServiceHandle) {
    std::panic::set_hook(Box::new(|panic_info| {
        #[allow(clippy::print_stderr)]
        {
            *TEST_FAILED.lock().unwrap() = -1;

            use std::backtrace::*;
            let bt = Backtrace::force_capture();

            eprintln!("\n\n{panic_info}\n");

            let str = format!("{bt}");
            let splits: Vec<_> = str.split('\n').collect();

            let mut start_i = 0;
            let mut i = 0;
            while i < splits.len() {
                if splits[i].ends_with("rust_begin_unwind") {
                    i += 1;
                    if i < splits.len() && splits[i].trim().starts_with("at ") {
                        i += 1;
                    }
                    start_i = i;
                }
                if splits[i].ends_with("core::panicking::panic_fmt") {
                    i += 1;
                    if i < splits.len() && splits[i].trim().starts_with("at ") {
                        i += 1;
                    }
                    start_i = i;
                    break;
                }
                i += 1;
            }

            let mut i = start_i;
            let n = 80;
            while i < n {
                let proc = if let Some(val) = splits.get(i) {
                    val.trim()
                } else {
                    break;
                };
                i += 1;

                let file_loc = if let Some(val) = splits.get(i) {
                    let val = val.trim();
                    if val.starts_with("at ") {
                        i += 1;
                        val
                    } else {
                        ""
                    }
                } else {
                    break;
                };

                eprintln!(
                    "  {}{}    {}",
                    if i < 20 { " " } else { "" },
                    proc,
                    file_loc
                );
            }
            if i == n {
                eprintln!("...");
            }

            eprintln!("\n\nInstruction sequence:");
            dump_test_instrs();

            #[cfg(not(feature = "viz_gui"))]
            std::process::abort();
        }
    }));

    tokio::task::spawn(test_format::instr_reader(internal_handle));
}

async fn push_staking_action_from_cmd_str(
    call: &TFLServiceCalls,
    cmd_str: &str,
) -> Result<(), String> {
    use zebra_chain::transaction::{LockTime, Transaction, UnminedTx};
    let staking_action = zcash_primitives::transaction::StakingAction::parse_from_cmd(cmd_str)?;
    let tx: UnminedTx = Transaction::VCrosslink {
        network_upgrade: zebra_chain::parameters::NetworkUpgrade::Nu6,
        lock_time: LockTime::unlocked(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        sapling_shielded_data: None,
        orchard_shielded_data: None,
        expiry_height: BlockHeight(0),
        staking_action,
    }
    .into();

    if let Ok(MempoolResponse::Queued(receivers)) =
        (call.mempool)(MempoolRequest::Queue(vec![tx.into()])).await
    {
        for receiver in receivers {
            match receiver {
                Err(err) => return Err(format!("tried to send command transaction: {err}")),
                Ok(receiver) => match receiver.await {
                    Err(err) => return Err(format!("tried to await command transaction: {err}")),
                    Ok(result) => match result {
                        Err(err) => {
                            return Err(format!("unsuccessfully mempooled transaction: {err}"))
                        }
                        Ok(()) => {}
                    },
                },
            }
        }
    }
    Ok(())
}

pub(crate) async fn tfl_service_main_loop(
    internal_handle: TFLServiceHandle,
) -> Result<(), String> {
    let call = internal_handle.call.clone();
    let config = internal_handle.config.clone();
    let params = &PROTOTYPE_PARAMETERS;

    if *TEST_MODE.lock().unwrap() {
        run_tfl_test(internal_handle.clone());
    }

    let public_ip_string = config
        .public_address
        .unwrap_or(String::from_str("/ip4/127.0.0.1/tcp/45869").unwrap());
    info!("public IP: {}", public_ip_string);

    let user_name = config
        .insecure_user_name
        .unwrap_or(public_ip_string.clone());
    info!("user_name: {}", user_name);

    let (_rng, my_private_key, my_public_key) =
        rng_private_public_key_from_address(user_name.as_bytes());
    internal_handle.internal.lock().await.my_public_key = my_public_key;

    // TODO: BFT consensus networking will be integrated into zebra-network.
    // For now, the tenderlink entry_point has been removed. The consensus engine
    // needs to be reimplemented using zebra-network's peer set infrastructure.
    info!("BFT consensus networking not yet integrated into zebra-network");

    let mut run_instant = Instant::now();
    let mut last_diagnostic_print = Instant::now();
    let mut current_bc_tip: Option<(BlockHeight, BlockHash)> = None;

    loop {
        let new_bc_tip = if let Ok(StateResponse::Tip(val)) = (call.state)(StateRequest::Tip).await
        {
            val
        } else {
            None
        };

        tokio::time::sleep_until(run_instant).await;
        run_instant += MAIN_LOOP_SLEEP_INTERVAL;

        #[allow(unused_mut)]
        let mut internal = internal_handle.internal.lock().await;

        if !internal.tfl_is_activated {
            if let Some((height, _hash)) = new_bc_tip {
                if height < TFL_ACTIVATION_HEIGHT {
                    continue;
                } else {
                    internal.tfl_is_activated = true;
                    info!("activating TFL!");
                }
            }
        }

        if last_diagnostic_print.elapsed() >= MAIN_LOOP_INFO_DUMP_INTERVAL {
            last_diagnostic_print = Instant::now();
            if let (Some((tip_height, _tip_hash)), Some((final_height, _final_hash))) =
                (current_bc_tip, internal.latest_final_block)
            {
                if tip_height < final_height {
                    info!(
                        "Our PoW tip is {} blocks away from the latest final block.",
                        final_height - tip_height
                    );
                } else {
                    let behind = tip_height - final_height;
                    if behind > 512 {
                        warn!("WARNING! BFT-Finality is falling behind the PoW chain. Current gap to tip is {:?} blocks.", behind);
                    }
                }
            }
        }

        current_bc_tip = new_bc_tip;
    }
}

async fn tfl_block_finality_from_height_hash(
    internal_handle: TFLServiceHandle,
    height: BlockHeight,
    hash: BlockHash,
) -> Result<Option<TFLBlockFinality>, TFLServiceError> {
    let call = internal_handle.call.clone();
    let (final_height, final_hash) = match tfl_final_block_height_hash(&internal_handle).await {
        Some(v) => v,
        None => {
            return Err(TFLServiceError::Misc(
                "There is no final block.".to_string(),
            ));
        }
    };

    if height > final_height {
        Ok(Some(TFLBlockFinality::NotYetFinalized))
    } else {
        let cmp_hash = if height == final_height {
            final_hash
        } else {
            match (call.state)(StateRequest::BlockHeader(height.into())).await {
                Ok(StateResponse::BlockHeader { hash, .. }) => hash,
                Err(err) => return Err(TFLServiceError::Misc(err.to_string())),
                _ => {
                    return Err(TFLServiceError::Misc(
                        "Invalid BlockHeader response type".to_string(),
                    ))
                }
            }
        };

        Ok(Some(if hash == cmp_hash {
            TFLBlockFinality::Finalized
        } else {
            TFLBlockFinality::CantBeFinalized
        }))
    }
}

pub(crate) async fn tfl_service_incoming_request(
    internal_handle: TFLServiceHandle,
    request: TFLServiceRequest,
) -> Result<TFLServiceResponse, TFLServiceError> {
    let call = internal_handle.call.clone();

    #[allow(unreachable_patterns)]
    match request {
        TFLServiceRequest::IsTFLActivated => Ok(TFLServiceResponse::IsTFLActivated(
            internal_handle.internal.lock().await.tfl_is_activated,
        )),

        TFLServiceRequest::FinalBlockHeightHash => Ok(TFLServiceResponse::FinalBlockHeightHash(
            tfl_final_block_height_hash(&internal_handle).await,
        )),

        TFLServiceRequest::FinalBlockRx => {
            let internal = internal_handle.internal.lock().await;
            Ok(TFLServiceResponse::FinalBlockRx(
                internal.final_change_tx.subscribe(),
            ))
        }

        TFLServiceRequest::SetFinalBlockHash(hash) => Ok(TFLServiceResponse::SetFinalBlockHash(
            tfl_set_finality_by_hash(internal_handle.clone(), hash).await,
        )),

        TFLServiceRequest::BlockFinalityStatus(height, hash) => {
            match tfl_block_finality_from_height_hash(internal_handle.clone(), height, hash).await {
                Ok(val) => Ok(TFLServiceResponse::BlockFinalityStatus(val)),
                Err(err) => Err(err),
            }
        }

        TFLServiceRequest::TxFinalityStatus(hash) => Ok(TFLServiceResponse::TxFinalityStatus({
            if let Ok(StateResponse::Transaction(Some(tx))) =
                (call.state)(StateRequest::Transaction(hash)).await
            {
                let (final_height, _final_hash) =
                    match tfl_final_block_height_hash(&internal_handle).await {
                        Some(v) => v,
                        None => {
                            return Err(TFLServiceError::Misc(
                                "There is no final block.".to_string(),
                            ));
                        }
                    };

                if tx.height <= final_height {
                    Some(TFLBlockFinality::Finalized)
                } else {
                    Some(TFLBlockFinality::NotYetFinalized)
                }
            } else {
                None
            }
        })),

        TFLServiceRequest::Roster => Ok(TFLServiceResponse::Roster({
            let internal = internal_handle.internal.lock().await;
            internal
                .validators_at_current_height
                .iter()
                .map(|v| (<[u8; 32]>::from(v.public_key), v.voting_power))
                .collect()
        })),

        TFLServiceRequest::FatPointerToBFTChainTip => {
            let internal = internal_handle.internal.lock().await;
            Ok(TFLServiceResponse::FatPointerToBFTChainTip(
                internal.fat_pointer_to_tip.clone(),
            ))
        }

        TFLServiceRequest::StakingCmd(cmd) => {
            match push_staking_action_from_cmd_str(&internal_handle.call, &cmd).await {
                Ok(()) => Ok(TFLServiceResponse::StakingCmd),
                Err(err) => Err(TFLServiceError::Misc(format!("{err}"))),
            }
        }

        _ => Err(TFLServiceError::NotImplemented),
    }
}

async fn tfl_set_finality_by_hash(
    internal_handle: TFLServiceHandle,
    hash: BlockHash,
) -> Option<BlockHeight> {
    let mut internal = internal_handle.internal.lock().await;

    if internal.tfl_is_activated {
        let new_height = block_height_from_hash(&internal_handle.call, hash).await;

        if let Some(height) = new_height {
            internal.latest_final_block = Some((height, hash));
        }

        new_height
    } else {
        None
    }
}

async fn tfl_block_sequence(
    call: &TFLServiceCalls,
    start_hash: BlockHash,
    final_height_hash: Option<(BlockHeight, BlockHash)>,
    include_start_hash: bool,
    read_extra_info: bool,
) -> (Vec<(BlockHeight, BlockHash)>, Vec<Option<Arc<Block>>>) {
    let (start_height, init_hash) = {
        if let Ok(StateResponse::BlockHeader { height, header, .. }) =
            (call.state)(StateRequest::BlockHeader(start_hash.into())).await
        {
            if include_start_hash {
                (Some(height), Some(header.previous_block_hash))
            } else {
                (Some(BlockHeight(height.0 + 1)), Some(start_hash))
            }
        } else {
            (None, None)
        }
    };
    let (final_height, final_hash) = if let Some((height, hash)) = final_height_hash {
        (Some(height), Some(hash))
    } else if let Ok(StateResponse::Tip(val)) = (call.state)(StateRequest::Tip).await {
        val.unzip()
    } else {
        (None, None)
    };

    if start_height.is_none() {
        error!(?start_hash, "start_hash has invalid height");
        return (Vec::new(), Vec::new());
    }
    let start_height = start_height.unwrap();
    let init_hash = init_hash.unwrap();

    if final_height.is_none() {
        error!(?final_height, "final_hash has invalid height");
        return (Vec::new(), Vec::new());
    }
    let final_height = final_height.unwrap();

    if final_height < start_height {
        error!(?final_height, ?start_height, "final_height < start_height");
        return (Vec::new(), Vec::new());
    }

    let mut hashes = Vec::with_capacity((final_height - start_height + 1) as usize);
    let mut chunk_i = 0;
    let mut chunk =
        Vec::with_capacity(zebra_state::constants::MAX_FIND_BLOCK_HASHES_RESULTS as usize);
    let mut c = 0;
    loop {
        if chunk_i >= chunk.len() {
            let chunk_start_hash = if chunk.is_empty() {
                &init_hash
            } else {
                chunk.last().expect("should have chunk elements by now")
            };

            let res = (call.state)(StateRequest::FindBlockHashes {
                known_blocks: vec![*chunk_start_hash],
                stop: final_hash,
            })
            .await;

            if let Ok(StateResponse::BlockHashes(chunk_hashes)) = res {
                if c == 0 && include_start_hash && !chunk_hashes.is_empty() {
                    assert_eq!(
                        chunk_hashes[0], start_hash,
                        "first hash is not the one requested"
                    );
                }

                chunk = chunk_hashes;
            } else {
                break;
            }

            chunk_i = 0;
        }

        if let Some(val) = chunk.get(chunk_i) {
            let height = BlockHeight(
                start_height.0 + <u32>::try_from(hashes.len()).expect("should fit in u32"),
            );
            hashes.push((height, *val));
        } else {
            break;
        };
        chunk_i += 1;
        c += 1;
    }

    let mut infos = Vec::with_capacity(if read_extra_info { hashes.len() } else { 0 });
    if read_extra_info {
        for hash in &hashes {
            infos.push(
                if let Ok(StateResponse::Block(block)) =
                    (call.state)(StateRequest::Block((hash.1).into())).await
                {
                    block
                } else {
                    None
                },
            )
        }
    }

    (hashes, infos)
}

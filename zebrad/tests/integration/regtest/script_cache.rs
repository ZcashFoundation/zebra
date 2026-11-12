//! Block verification of transactions whose transparent scripts the mempool has already verified,
//! on a real Regtest node.

use std::{sync::Arc, time::Duration};

use color_eyre::eyre::{eyre, Result};

use zebra_chain::{
    amount::Amount,
    block::{self, Block, Height},
    parameters::{testnet::ConfiguredActivationHeights, Network, NetworkUpgrade},
    transaction::{self, LockTime, Transaction},
    transparent,
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::{
    client::{BlockTemplateResponse, BlockTemplateTimeSource},
    proposal_block_from_template,
    server::OPENED_RPC_ENDPOINT_MSG,
};
use zebra_test::{args, prelude::*};

use super::nu7::{mempool, rejected, send};
use crate::common::{
    config::{os_assigned_rpc_port_config, read_listen_addr_from_logs, testdir},
    launch::{wait_for_mempool_activation, ZebradTestDirExt, LAUNCH_DELAY},
    regtest::MiningRpcMethods,
};

/// Pushes the redeem script `OP_TRUE`, which spends outputs paid to [`op_true_address`].
const OP_TRUE_UNLOCK: [u8; 2] = [0x01, 0x51];

/// The ZIP-317 conventional fee for one transparent input and output.
const FEE: u64 = 10_000;

fn op_true_address(network: &Network) -> transparent::Address {
    transparent::Address::from_script_hash(
        network.kind(),
        hex_literal::hex!("da1745e9b549bd0bfa1a569971c77eba30cd5a4b"),
    )
}

/// Spends the output of `previous` paid to [`op_true_address`] back to the same address.
fn spend(
    network: &Network,
    previous: &Transaction,
    unlock_script: &[u8],
    lock_time: LockTime,
) -> Result<Transaction> {
    let (index, mut output) = previous
        .outputs()
        .into_iter()
        .enumerate()
        .find(|(_, output)| output.lock_script == op_true_address(network).script())
        .ok_or_else(|| eyre!("the coinbase pays the configured miner address"))?;
    let input = transparent::Input::PrevOut {
        outpoint: transparent::OutPoint {
            hash: previous.hash(),
            index: index.try_into()?,
        },
        unlock_script: transparent::Script::new(unlock_script),
        // A final sequence number would disable the lock time.
        sequence: 0,
    };
    output.value = (output.value - Amount::try_from(FEE)?)?;

    Ok(Transaction::test_v5(
        NetworkUpgrade::Nu5,
        vec![input],
        vec![output],
        lock_time,
        Height(200),
    ))
}

/// Waits for a template on `parent` that contains exactly `transactions`.
async fn template(
    client: &RpcRequestClient,
    network: &Network,
    parent: block::Hash,
    transactions: &[transaction::Hash],
) -> Result<Block> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let template: BlockTemplateResponse = client
                .json_result_from_call("getblocktemplate", "[]")
                .await
                .map_err(|err| eyre!(err))?;
            let mut hashes: Vec<_> = template.transactions().iter().map(|tx| tx.hash()).collect();
            hashes.sort();
            if template.previous_block_hash() == parent && hashes == transactions {
                return Ok(proposal_block_from_template(
                    &template,
                    BlockTemplateTimeSource::default(),
                    network,
                )?);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| eyre!("no template on {parent} with the mempool transactions"))?
}

/// Returns a copy of `block` with a later time than any template the node served for its parent.
///
/// The node reuses the verdicts of successful proposal checks, including the checks of the
/// templates it serves, for blocks that differ from them only in their nonce and solution.
/// Templates use the current time or the parent's minimum time, so a time later than both makes
/// the node verify this block's transactions. It stays well within the consensus time limits.
fn retimed(block: &Block) -> Result<Block> {
    let mut block = block.clone();
    let later = block
        .header
        .time
        .timestamp()
        .max(chrono::Utc::now().timestamp())
        + 10;
    Arc::make_mut(&mut block.header).time = chrono::DateTime::from_timestamp(later, 0)
        .ok_or_else(|| eyre!("block times are valid timestamps"))?;
    Ok(block)
}

/// Returns the hash of the best chain block at `height`.
async fn best_hash(client: &RpcRequestClient, height: u32) -> Result<block::Hash> {
    Ok(client
        .get_block(height.try_into()?)
        .await
        .map_err(|err| eyre!(err))?
        .ok_or_else(|| eyre!("no best chain block at height {height}"))?
        .hash())
}

/// Blocks containing transactions from the mempool are accepted and rejected exactly as they
/// would be if the mempool had not already verified their transparent scripts.
///
/// The transactions are admitted by the node's mempool, which remembers their successful script
/// verifications, and the blocks are checked by its block verifier and state, never by reusing an
/// earlier check of the same block. Covers:
/// - authorizing data that differs from the admitted transaction, under the same transaction ID,
/// - a block time before an admitted transaction's lock time,
/// - acceptance of a block that mines the admitted transactions.
#[tokio::test(flavor = "multi_thread")]
async fn mempool_verified_scripts_do_not_skip_block_checks() -> Result<()> {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(300), async {
        let network = Network::new_regtest(
            ConfiguredActivationHeights {
                nu5: Some(1),
                ..Default::default()
            }
            .into(),
        );
        let mut config = os_assigned_rpc_port_config(false, &network)?;
        config.mempool.debug_enable_at_height = Some(0);
        config.mining.miner_address = Some(op_true_address(&network).to_string().parse()?);
        let mut child = testdir()?
            .with_config(&mut config)?
            .spawn_child(args!["start"])?
            .with_timeout(LAUNCH_DELAY)?;
        let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;
        let client = RpcRequestClient::new(rpc_address);
        wait_for_mempool_activation(&client).await?;

        // Mature the coinbases of blocks 1 and 2.
        let tip = Height(transparent::MIN_TRANSPARENT_COINBASE_MATURITY + 1);
        let tip_hash = *client
            .generate(tip.0)
            .await?
            .last()
            .ok_or_else(|| eyre!("generate returns the new block hashes"))?;
        let mut coinbases = Vec::new();
        for height in 1..=2 {
            let block = client
                .get_block(height)
                .await
                .map_err(|err| eyre!(err))?
                .ok_or_else(|| eyre!("generated blocks are readable"))?;
            coinbases.push(block.transactions[0].clone());
        }
        let mut times = Vec::new();
        for height in tip.0 - 10..=tip.0 {
            let block = client
                .get_block(height.try_into()?)
                .await
                .map_err(|err| eyre!(err))?
                .ok_or_else(|| eyre!("generated blocks are readable"))?;
            times.push(block.header.time);
        }
        times.sort();
        let median_time_past = times[5];

        // The mempool admits the lock time because it is before the next block's median time
        // past, but a block at that time doesn't satisfy it.
        let lock_time = median_time_past - chrono::Duration::seconds(1);
        let unlocked = spend(
            &network,
            &coinbases[0],
            &OP_TRUE_UNLOCK,
            LockTime::unlocked(),
        )?;
        let locked = spend(
            &network,
            &coinbases[1],
            &OP_TRUE_UNLOCK,
            LockTime::Time(lock_time),
        )?;
        for tx in [&unlocked, &locked] {
            send(&client, tx).await?;
        }
        let mut admitted = vec![unlocked.hash(), locked.hash()];
        admitted.sort();
        mempool(&client, &admitted).await?;
        let block = template(&client, &network, tip_hash, &admitted).await?;

        // The admitted transaction's ID still matches, but its authorizing data differs, so its
        // script must be verified again rather than answered from the mempool's success, which
        // would leave only the block commitment check to fail. Proposal checks never reuse earlier
        // verdicts, so they can start from the template.
        let mut forged = block.clone();
        let position = forged
            .transactions
            .iter()
            .position(|tx| tx.hash() == unlocked.hash())
            .ok_or_else(|| eyre!("the template contains the unlocked transaction"))?;
        let invalid_unlock = spend(&network, &coinbases[0], &[0x01, 0x00], LockTime::unlocked())?;
        assert_eq!(invalid_unlock.hash(), unlocked.hash());
        assert_ne!(invalid_unlock.auth_digest(), unlocked.auth_digest());
        forged.transactions[position] = Arc::new(invalid_unlock);
        let reason = rejected(&client, &forged).await?;
        assert!(
            reason.contains("script") && !reason.contains("commitment"),
            "changed authorizing data must fail transaction verification: {reason}"
        );

        // The lock time is checked against this block's time, not the mempool's.
        let mut early = block.clone();
        Arc::make_mut(&mut early.header).time = lock_time;
        let reason = rejected(&client, &early).await?;
        assert!(
            reason.contains("lockeduntilafterblocktime"),
            "a block before the lock time must fail the lock time check: {reason}"
        );

        // A block on the same parent mines the admitted transactions. Proposal reuse ignores only
        // the nonce and solution, so a later time than the served template makes the node verify
        // the block's transactions instead of reusing a template check.
        let mined = retimed(&block)?;
        assert!(mined.header.time > block.header.time);
        client.submit_block(mined.clone()).await?;
        assert_eq!(best_hash(&client, tip.0 + 1).await?, mined.hash());
        mempool(&client, &[]).await?;

        child.kill(false)?;
        let output = child.wait_with_output()?;
        output.assert_was_killed()?;
        output.assert_failure()?;
        Ok(())
    })
    .await?
}

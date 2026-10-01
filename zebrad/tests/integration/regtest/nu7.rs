//! End-to-end checks of the NU7 consensus changes on a running Regtest node: ZIP 218 subsidy and
//! shielded action limits, ZIP 207/214 funding streams and address periods, ZIP 2003 transaction
//! versions, ZIP 235 fee burning and one-time lockbox disbursements, across the activation height.
//!
//! See <https://github.com/ZcashFoundation/zebra/issues/11549>.

use std::{collections::HashSet, sync::Arc, time::Duration};

use color_eyre::eyre::{eyre, Result};
use tower::ServiceExt;

use zebra_chain::{
    amount::Amount,
    block::{Block, Height},
    parameters::{testnet::ConfiguredActivationHeights, Network, NetworkUpgrade},
    serialization::ZcashSerialize as _,
    transaction::{self, LockTime, Transaction},
    transparent,
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::{
    client::{SubmitBlockErrorResponse, SubmitBlockResponse},
    methods::SendRawTransactionResponse,
    server::OPENED_RPC_ENDPOINT_MSG,
};
use zebra_test::{args, prelude::*};

use crate::common::{
    config::{os_assigned_rpc_port_config, read_listen_addr_from_logs, testdir},
    launch::{ZebradTestDirExt, LAUNCH_DELAY},
    regtest::MiningRpcMethods,
};

// A standard P2SH output whose redeem script is OP_TRUE. These spends still pass through the
// real UTXO, maturity, standardness, script, fee, and transaction-version checks.
fn spend(previous: &Transaction, nu7: bool, fee: u64) -> Result<Transaction> {
    let input = transparent::Input::PrevOut {
        outpoint: transparent::OutPoint {
            hash: previous.hash(),
            index: 0,
        },
        unlock_script: transparent::Script::new(&[0x01, 0x51]),
        sequence: u32::MAX,
    };
    let mut output = previous.outputs()[0].clone();
    output.value = (output.value - Amount::try_from(fee)?)?;
    Ok(if nu7 {
        Transaction::test_v5(
            NetworkUpgrade::Nu7,
            vec![input],
            vec![output],
            LockTime::unlocked(),
            Height(105),
        )
    } else {
        Transaction::test_v4(vec![input], vec![output], LockTime::unlocked(), Height(200))
    })
}

async fn send(client: &RpcRequestClient, tx: &Transaction) -> Result<()> {
    let data = hex::encode(tx.zcash_serialize_to_vec()?);
    let response: SendRawTransactionResponse = client
        .json_result_from_call("sendrawtransaction", format!(r#"["{data}"]"#))
        .await
        .map_err(|err| eyre!(err))?;
    assert_eq!(response, SendRawTransactionResponse::new(tx.hash()));
    Ok(())
}

async fn mempool(client: &RpcRequestClient, expected: &[transaction::Hash]) -> Result<()> {
    let expected: HashSet<_> = expected.iter().map(ToString::to_string).collect();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let actual: HashSet<String> = client
                .json_result_from_call("getrawmempool", "[]")
                .await
                .map_err(|err| eyre!(err))?;
            if actual == expected {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| eyre!("mempool did not reach the expected transaction set: {expected:?}"))?
}

/// V4 admission follows the next block's upgrade, including eviction at the NU7 boundary.
#[tokio::test(flavor = "multi_thread")]
async fn nu7_v4_mempool_activation() -> Result<()> {
    use zebra_chain::parameters::testnet::RegtestParameters;

    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(300), async {
        let network = Network::new_regtest(RegtestParameters {
            activation_heights: ConfiguredActivationHeights {
                canopy: Some(1),
                nu5: Some(2),
                nu6: Some(4),
                nu6_1: Some(5),
                nu6_2: Some(6),
                nu6_3: Some(7),
                nu7: Some(105),
                ..Default::default()
            },
            should_allow_unshielded_coinbase_spends: Some(true),
            ..Default::default()
        });
        let mut config = os_assigned_rpc_port_config(false, &network)?;
        config.mempool.debug_enable_at_height = Some(0);
        config.mining.miner_address = Some(
            transparent::Address::from_script_hash(
                network.kind(),
                hex_literal::hex!("da1745e9b549bd0bfa1a569971c77eba30cd5a4b"),
            )
            .to_string()
            .parse()?,
        );
        let mut child = testdir()?
            .with_config(&mut config)?
            .spawn_child(args!["start"])?;
        let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;
        tokio::time::sleep(LAUNCH_DELAY).await;
        let client = RpcRequestClient::new(rpc_address);

        let (first, height) = client.block_from_template(&network).await?;
        assert_eq!(height, Height(1));
        let coinbase = first.transactions[0].clone();
        client.submit_block(first).await?;
        for expected_height in 2..=103 {
            let (block, height) = client.block_from_template(&network).await?;
            assert_eq!(height, Height(expected_height));
            client.submit_block(block).await?;
        }

        // Save an empty block so the admitted V4 transaction remains unmined at the boundary.
        let (boundary, height) = client.block_from_template(&network).await?;
        assert_eq!(height, Height(104));
        let v4 = spend(&coinbase, false, 10_001)?;
        send(&client, &v4).await?;
        mempool(&client, &[v4.hash()]).await?;
        client.submit_block(boundary).await?;
        mempool(&client, &[]).await?;

        // A fresh transaction avoids the rejection cache; its mature input is still unspent.
        let rejected_v4 = spend(&coinbase, false, 10_002)?;
        let data = hex::encode(rejected_v4.zcash_serialize_to_vec()?);
        let response: serde_json::Value = serde_json::from_str(
            &client
                .text_from_call("sendrawtransaction", format!(r#"["{data}"]"#))
                .await?,
        )?;
        assert_eq!(
            response["error"]["code"],
            i32::from(zebra_rpc::server::error::LegacyCode::Verify),
        );
        mempool(&client, &[]).await?;

        // The same input remains spendable as V5, isolating rejection to the version rule.
        let v5 = spend(&coinbase, true, 10_002)?;
        send(&client, &v5).await?;
        mempool(&client, &[v5.hash()]).await?;

        child.kill(false)?;
        let output = child.wait_with_output()?;
        output.assert_was_killed()?;
        output.assert_failure()?;
        Ok(())
    })
    .await?
}

/// Real mempool admission, aggregate fees, and parent-dependent NU7 mining survive reorgs and restart.
#[tokio::test(flavor = "multi_thread")]
async fn nu7_nsm_mining_reorg_and_restart() -> Result<()> {
    use zebra_chain::{
        amount::NonNegative,
        parameters::{
            subsidy::{additional_block_subsidy, scheduled_block_subsidy},
            testnet::RegtestParameters,
        },
    };
    use zebra_rpc::{
        client::{BlockProposalResponse, BlockTemplateResponse, GetBlockSubsidyResponse},
        proposal_block_from_template,
    };

    async fn template(
        client: &RpcRequestClient,
        height: u32,
        expected: &[transaction::Hash],
    ) -> Result<BlockTemplateResponse> {
        let expected: HashSet<_> = expected.iter().copied().collect();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let template: BlockTemplateResponse = client
                    .json_result_from_call("getblocktemplate", "[]")
                    .await
                    .map_err(|err| eyre!(err))?;
                let actual: HashSet<_> =
                    template.transactions().iter().map(|tx| tx.hash()).collect();
                if template.height() == height && actual == expected {
                    return Ok(template);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .map_err(|_| eyre!("no height-{height} template with transactions {expected:?}"))?
    }

    async fn proposal(client: &RpcRequestClient, block: &Block) -> Result<BlockProposalResponse> {
        let data = hex::encode(block.zcash_serialize_to_vec()?);
        client
            .json_result_from_call(
                "getblocktemplate",
                format!(r#"[{{"mode":"proposal","data":"{data}"}}]"#),
            )
            .await
            .map_err(|err| eyre!(err))
    }

    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(300), async {
        let seed = Amount::<NonNegative>::try_from(100_000_000)?;
        let network = Network::new_regtest(RegtestParameters {
            activation_heights: ConfiguredActivationHeights {
                canopy: Some(1),
                nu5: Some(2),
                nu6: Some(4),
                nu6_1: Some(5),
                nu6_2: Some(6),
                nu6_3: Some(7),
                nu7: Some(105),
                ..Default::default()
            },
            // The reserve is configured, not derived from historical coinbase underclaims.
            initial_nsm_value_balance: Some(seed),
            nsm_reissuance_height: Some(Height(106)),
            should_allow_unshielded_coinbase_spends: Some(true),
            ..Default::default()
        });
        let mut config = os_assigned_rpc_port_config(false, &network)?;
        config.state.ephemeral = false;
        config.state.should_backup_non_finalized_state = true;
        // This selects synchronous backups on block commits, rather than disabling persistence.
        config.state.debug_skip_non_finalized_state_backup_task = true;
        config.mempool.debug_enable_at_height = Some(0);
        config.mining.miner_address = Some(
            transparent::Address::from_script_hash(
                network.kind(),
                hex_literal::hex!("da1745e9b549bd0bfa1a569971c77eba30cd5a4b"),
            )
            .to_string()
            .parse()?,
        );
        let mut child = testdir()?
            .with_config(&mut config)?
            .spawn_child(args!["start"])?;
        let test_dir = child
            .dir
            .take()
            .expect("test directory is retained for restart");
        let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;
        tokio::time::sleep(LAUNCH_DELAY).await;
        let client = RpcRequestClient::new(rpc_address);
        let coinbase_value = |block: &Block| {
            block.transactions[0]
                .outputs()
                .iter()
                .map(|output| output.value)
                .sum::<Result<Amount<NonNegative>, _>>()
        };

        // Mature two real coinbase outputs before approaching the NU7 boundary.
        let mut coinbases = Vec::new();
        for expected_height in 1..=102 {
            let (block, height) = client.block_from_template(&network).await?;
            assert_eq!(height, Height(expected_height));
            if expected_height <= 2 {
                coinbases.push(block.transactions[0].clone());
            }
            client.submit_block(block).await?;
        }

        let before = [
            spend(&coinbases[0], false, 10_001)?,
            spend(&coinbases[1], false, 10_001)?,
        ];
        for tx in &before {
            send(&client, tx).await?;
        }
        let before_ids = before.each_ref().map(Transaction::hash);
        mempool(&client, &before_ids).await?;
        let before_template = template(&client, 103, &before_ids).await?;
        for tx in before_template.transactions() {
            assert_eq!(u64::from(tx.fee()), 10_001);
        }
        assert_eq!(i64::from(before_template.coinbase_txn().fee()), -20_002);
        let before_block = proposal_block_from_template(&before_template, None, &network)?;
        assert_eq!(
            coinbase_value(&before_block)?,
            (scheduled_block_subsidy(Height(103), &network)? + Amount::try_from(20_002)?)?,
            "all gross fees are claimable before NU7",
        );
        client.submit_block(before_block).await?;
        mempool(&client, &[]).await?;

        let (boundary, height) = client.block_from_template(&network).await?;
        assert_eq!(height, Height(104));
        client.submit_block(boundary).await?;
        mempool(&client, &[]).await?;
        let empty_activation_template = template(&client, 105, &[]).await?;
        let empty_activation =
            proposal_block_from_template(&empty_activation_template, None, &network)?;
        assert_eq!(
            coinbase_value(&empty_activation)?,
            scheduled_block_subsidy(Height(105), &network)?,
            "seeding and reissuance have distinct configured heights",
        );

        // These spends expire at height 105, so reorgs cannot select them in the height-106
        // templates used to compare the two parent reserves.
        let fees = [
            spend(&before[0], true, 10_001)?,
            spend(&before[1], true, 10_001)?,
        ];
        for tx in &fees {
            send(&client, tx).await?;
        }
        let fee_ids = fees.each_ref().map(Transaction::hash);
        mempool(&client, &fee_ids).await?;
        let fee_template = template(&client, 105, &fee_ids).await?;
        for tx in fee_template.transactions() {
            assert_eq!(u64::from(tx.fee()), 10_001);
        }
        assert_eq!(i64::from(fee_template.coinbase_txn().fee()), -8_001);
        let fee_block = proposal_block_from_template(&fee_template, None, &network)?;
        assert_eq!(
            coinbase_value(&fee_block)?,
            (scheduled_block_subsidy(Height(105), &network)? + Amount::try_from(8_001)?)?,
            "aggregate gross fees 20002 contribute 12001, not two rounded contributions of 6000",
        );
        assert_eq!(
            proposal(&client, &fee_block).await?,
            BlockProposalResponse::Valid
        );
        client.submit_block(fee_block.clone()).await?;
        mempool(&client, &[]).await?;

        let fee_reserve = (seed + Amount::try_from(12_001)?)?;
        let first_additional = additional_block_subsidy(Height(106), &network, fee_reserve);
        let expected = (scheduled_block_subsidy(Height(106), &network)? + first_additional)?;
        let high_template = template(&client, 106, &[]).await?;
        assert_eq!(high_template.previous_block_hash(), fee_block.hash());
        let high_block = proposal_block_from_template(&high_template, None, &network)?;
        assert_eq!(coinbase_value(&high_block)?, expected);
        assert_eq!(
            proposal(&client, &high_block).await?,
            BlockProposalResponse::Valid
        );
        let high_subsidy: GetBlockSubsidyResponse = client
            .json_result_from_call("getblocksubsidy", "[106]")
            .await
            .map_err(|err| eyre!(err))?;
        assert_eq!(high_subsidy.total_block_subsidy(), expected);

        let mut overclaim = high_block.clone();
        let coinbase = Arc::make_mut(&mut overclaim.transactions[0]);
        let mut outputs = coinbase.outputs();
        outputs[0].value = (outputs[0].value + Amount::try_from(1)?)?;
        *coinbase = coinbase.clone().with_transparent_outputs(outputs);
        // Transparent outputs change the txid but not the authorizing-data commitment.
        Arc::make_mut(&mut overclaim.header).merkle_root = overclaim.transactions.iter().collect();
        let encoded = hex::encode(overclaim.zcash_serialize_to_vec()?);
        let response: SubmitBlockResponse = client
            .json_result_from_call("submitblock", format!(r#"["{encoded}"]"#))
            .await
            .map_err(|err| eyre!(err))?;
        assert_eq!(
            response,
            SubmitBlockResponse::ErrorResponse(SubmitBlockErrorResponse::Rejected),
            "contextual validation must reject excess NSM issuance",
        );

        // Switch between real parents at the SAME height, with and without fee reserve credit.
        let fee_params = serde_json::to_string(&[fee_block.hash().to_string()])?;
        let _: () = client
            .json_result_from_call("invalidateblock", &fee_params)
            .await
            .map_err(|err| eyre!(err))?;
        client.submit_block(empty_activation.clone()).await?;
        let low_template = template(&client, 106, &[]).await?;
        assert_eq!(low_template.previous_block_hash(), empty_activation.hash());
        let low_block = proposal_block_from_template(&low_template, None, &network)?;
        let low_expected = (scheduled_block_subsidy(Height(106), &network)?
            + additional_block_subsidy(Height(106), &network, seed))?;
        assert_ne!(
            low_expected, expected,
            "the parent reserve must change the payout"
        );
        assert_eq!(coinbase_value(&low_block)?, low_expected);
        assert_eq!(
            proposal(&client, &low_block).await?,
            BlockProposalResponse::Valid
        );
        let low_subsidy: GetBlockSubsidyResponse = client
            .json_result_from_call("getblocksubsidy", "[106]")
            .await
            .map_err(|err| eyre!(err))?;
        assert_eq!(low_subsidy.total_block_subsidy(), low_expected);
        let mut stale_payout = low_block.clone();
        stale_payout.transactions[0] = high_block.transactions[0].clone();
        Arc::make_mut(&mut stale_payout.header).merkle_root =
            stale_payout.transactions.iter().collect();
        assert!(
            !proposal(&client, &stale_payout).await?.is_valid(),
            "the prior parent's higher payout must not be accepted on the lower-reserve parent",
        );

        // Reconsider first: the invalidation cache retains only one branch per height.
        let reconsidered: Vec<zebra_chain::block::Hash> = client
            .json_result_from_call("reconsiderblock", &fee_params)
            .await
            .map_err(|err| eyre!(err))?;
        assert_eq!(reconsidered, [fee_block.hash()]);
        let empty_params = serde_json::to_string(&[empty_activation.hash().to_string()])?;
        let _: () = client
            .json_result_from_call("invalidateblock", &empty_params)
            .await
            .map_err(|err| eyre!(err))?;
        let restored_template = template(&client, 106, &[]).await?;
        assert_eq!(restored_template.previous_block_hash(), fee_block.hash());
        let restored = proposal_block_from_template(&restored_template, None, &network)?;
        assert_eq!(coinbase_value(&restored)?, expected);
        assert_eq!(
            proposal(&client, &restored).await?,
            BlockProposalResponse::Valid
        );
        client.submit_block(restored.clone()).await?;
        let reserve = (fee_reserve - first_additional)?;

        let params = serde_json::to_string(&[restored.hash().to_string()])?;
        let _: () = client
            .json_result_from_call("invalidateblock", &params)
            .await
            .map_err(|err| eyre!(err))?;
        let replacement_template = template(&client, 106, &[]).await?;
        let replacement = proposal_block_from_template(&replacement_template, None, &network)?;
        assert_eq!(
            coinbase_value(&replacement)?,
            expected,
            "rollback restores reserve"
        );
        let reconsidered: Vec<zebra_chain::block::Hash> = client
            .json_result_from_call("reconsiderblock", &params)
            .await
            .map_err(|err| eyre!(err))?;
        assert_eq!(reconsidered, [restored.hash()]);

        // Reconsideration acknowledges before publishing the tip and its synchronous backup.
        let persisted_template = template(&client, 107, &[]).await?;
        assert_eq!(persisted_template.previous_block_hash(), restored.hash());
        #[cfg(unix)]
        {
            let pid = child
                .child
                .as_ref()
                .expect("the node is still owned until shutdown")
                .id();
            crate::common::zcashd_compat::launch::send_signal(pid, "-TERM")?;
        }
        #[cfg(not(unix))]
        child.kill(true)?;
        let output = child.wait_with_output()?;
        #[cfg(unix)]
        output.assert_success()?;
        #[cfg(not(unix))]
        output.assert_was_killed()?;
        let version = zebra_state::state_database_format_version_on_disk(&config.state, &network)
            .map_err(|err| eyre!(err))?
            .expect("persistent node must create a database version");
        assert_eq!(
            version.major, 29,
            "the ordinary node must use the v29 database"
        );

        // Reopen the actual node's state, including its synchronous non-finalized backup.
        // Checking the exact tip prevents a finalized-only read from falsely proving persistence.
        // State-owned metrics tasks retain the database until their runtime shuts down.
        let state_config = config.state.clone();
        let inspection_network = network.clone();
        let restored_hash = restored.hash();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime.block_on(async move {
                let (state, read_state, _, _) =
                    zebra_state::init(state_config, &inspection_network, Height(0), 1).await;
                let zebra_state::ReadResponse::TipPoolValues {
                    tip_height,
                    tip_hash,
                    value_balance,
                } = read_state
                    .clone()
                    .oneshot(zebra_state::ReadRequest::TipPoolValues)
                    .await
                    .map_err(|err| eyre!(err))?
                else {
                    panic!("TipPoolValues must return the restored chain tip and its pools");
                };
                assert_eq!((tip_height, tip_hash), (Height(106), restored_hash));
                assert_eq!(value_balance.nsm_amount(), reserve);
                for (height, expected_reserve) in [(104, Amount::zero()), (105, fee_reserve)] {
                    let zebra_state::ReadResponse::BlockInfo(Some(info)) = read_state
                        .clone()
                        .oneshot(zebra_state::ReadRequest::BlockInfo(Height(height).into()))
                        .await
                        .map_err(|err| eyre!(err))?
                    else {
                        panic!("the node's persisted chain must contain height {height}");
                    };
                    assert_eq!(info.value_pools().nsm_amount(), expected_reserve);
                }
                drop((state, read_state));
                Ok(())
            })
        })
        .await??;

        let mut child = test_dir.spawn_child(args!["start"])?;
        let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;
        tokio::time::sleep(LAUNCH_DELAY).await;
        let client = RpcRequestClient::new(rpc_address);
        let restarted_template = template(&client, 107, &[]).await?;
        assert_eq!(restarted_template.previous_block_hash(), restored.hash());
        let block = proposal_block_from_template(&restarted_template, None, &network)?;
        assert_eq!(
            coinbase_value(&block)?,
            (scheduled_block_subsidy(Height(107), &network)?
                + additional_block_subsidy(Height(107), &network, reserve))?,
            "restart retains the aggregate fee credit and prior reserve debit",
        );
        assert_eq!(
            proposal(&client, &block).await?,
            BlockProposalResponse::Valid
        );
        client.submit_block(block).await?;
        child.kill(false)?;
        child.wait_with_output()?.assert_was_killed()?;
        Ok(())
    })
    .await?
}

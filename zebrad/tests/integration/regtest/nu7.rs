//! End-to-end checks of the NU7 consensus changes on a running Regtest node: ZIP 218 subsidy and
//! shielded action limits, ZIP 207/214 funding streams and address periods, ZIP 2003 transaction
//! versions, ZIP 235 fee burning and one-time lockbox disbursements, across the activation height.
//!
//! See <https://github.com/ZcashFoundation/zebra/issues/11549>.

use std::{collections::HashSet, sync::Arc, time::Duration};

use color_eyre::eyre::{eyre, Result};
use tower::ServiceExt;

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{Block, Height},
    parameters::{
        subsidy::{
            funding_stream_address, funding_stream_values, miner_subsidy, scheduled_block_subsidy,
            FundingStreamReceiver,
        },
        testnet::{ConfiguredActivationHeights, ConfiguredFundingStreamRecipient},
        Network, NetworkKind, NetworkUpgrade,
    },
    serialization::ZcashSerialize as _,
    transaction::{self, LockTime, Transaction},
    transparent,
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::{
    client::{
        BlockProposalResponse, GetBlockSubsidyResponse, SubmitBlockErrorResponse,
        SubmitBlockResponse,
    },
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
    spend_expiring(previous, nu7, fee, Height(if nu7 { 105 } else { 200 }))
}

fn spend_expiring(
    previous: &Transaction,
    nu7: bool,
    fee: u64,
    expiry: Height,
) -> Result<Transaction> {
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
            expiry,
        )
    } else {
        Transaction::test_v4(vec![input], vec![output], LockTime::unlocked(), expiry)
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
    use zebra_rpc::{client::BlockTemplateResponse, proposal_block_from_template};

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

/// The kebab-cased reason a block proposal was rejected for.
async fn rejected(client: &RpcRequestClient, block: &Block) -> Result<String> {
    match proposal(client, block).await? {
        BlockProposalResponse::Rejected(reason) => Ok(reason),
        BlockProposalResponse::Valid => Err(eyre!("the proposal was accepted")),
    }
}

fn stream_recipient(
    receiver: FundingStreamReceiver,
    numerator: u64,
    addresses: Option<Vec<String>>,
) -> ConfiguredFundingStreamRecipient {
    ConfiguredFundingStreamRecipient {
        receiver,
        numerator,
        addresses,
    }
}

fn zat(zec: impl Into<Amount<NonNegative>>) -> u64 {
    u64::from(zec.into())
}

// `GetBlockchainInfoResponse` parses ZEC floats, which fail on values like 7.99999998 ZEC.
async fn chain_info(client: &RpcRequestClient) -> Result<serde_json::Value> {
    client
        .json_result_from_call("getblockchaininfo", "[]")
        .await
        .map_err(|err| eyre!(err))
}

/// The lockbox chain value pool at the tip, in zatoshis.
async fn lockbox_pool(client: &RpcRequestClient) -> Result<u64> {
    let info = chain_info(client).await?;
    let pool = (info["valuePools"].as_array().expect("array").iter())
        .find(|pool| pool["id"] == "lockbox")
        .expect("the lockbox pool is listed");
    Ok(pool["chainValueZat"].as_u64().expect("zatoshis"))
}

fn with_coinbase_outputs(block: &Block, outputs: Vec<transparent::Output>) -> Block {
    let mut block = block.clone();
    let coinbase = Arc::make_mut(&mut block.transactions[0]);
    *coinbase = coinbase.clone().with_transparent_outputs(outputs);
    Arc::make_mut(&mut block.header).merkle_root = block.transactions.iter().collect();
    block
}

/// Checks `getblocksubsidy` at `height` against the zebra-chain subsidy and stream functions.
async fn checked_subsidy(
    client: &RpcRequestClient,
    network: &Network,
    height: u32,
) -> Result<GetBlockSubsidyResponse> {
    let total = scheduled_block_subsidy(Height(height), network)?;
    let response: GetBlockSubsidyResponse = client
        .json_result_from_call("getblocksubsidy", format!("[{height}]"))
        .await
        .map_err(|err| eyre!(err))?;
    assert_eq!(response.total_block_subsidy(), total, "subsidy at {height}");
    assert_eq!(
        response.miner(),
        miner_subsidy(Height(height), network, total)?
    );
    let values = funding_stream_values(Height(height), network, total)?;
    let lockbox = values.get(&FundingStreamReceiver::Deferred).copied();
    assert_eq!(response.lockbox_total(), lockbox.unwrap_or_default());
    let key = |address: Option<&transparent::Address>, value: u64| {
        (value, address.map(ToString::to_string))
    };
    let mut expected: Vec<_> = (values.iter())
        .map(|(&receiver, &value)| {
            let address = funding_stream_address(Height(height), network, receiver);
            key(address, u64::from(value))
        })
        .collect();
    let mut actual: Vec<_> = (response.funding_streams().iter())
        .chain(response.lockbox_streams())
        .map(|stream| key(stream.address.as_ref(), u64::from(stream.value_zat)))
        .collect();
    expected.sort();
    actual.sort();
    assert_eq!(actual, expected, "funding streams at {height}");
    Ok(response)
}

/// Checks the stored coinbase at `height` pays exactly the miner, the transparent streams in
/// `subsidy` and the `extra` outputs: the lockbox stream never has an output.
async fn checked_coinbase(
    client: &RpcRequestClient,
    miner: &transparent::Address,
    height: u32,
    subsidy: &GetBlockSubsidyResponse,
    extra: &[(transparent::Address, u64)],
) -> Result<()> {
    let stored = client
        .get_block(i32::try_from(height)?)
        .await
        .map_err(|err| eyre!(err))?
        .expect("the submitted block is in the best chain");
    let mut expected = vec![(miner.script(), zat(subsidy.miner()))];
    expected.extend(subsidy.funding_streams().iter().map(|stream| {
        let address = stream
            .address
            .as_ref()
            .expect("transparent streams have addresses");
        (address.script(), u64::from(stream.value_zat))
    }));
    expected.extend(
        extra
            .iter()
            .map(|(address, value)| (address.script(), *value)),
    );
    let mut outputs: Vec<_> = (stored.transactions[0].outputs().iter())
        .map(|output| (output.lock_script.clone(), u64::from(output.value)))
        .collect();
    expected.sort();
    outputs.sort();
    assert_eq!(outputs, expected, "coinbase at {height}");
    Ok(())
}

/// ZIP 218 subsidy, ZIP 207/214 funding streams and the stretched address period across NU7.
#[tokio::test(flavor = "multi_thread")]
async fn nu7_subsidy_and_funding_streams_across_activation() -> Result<()> {
    use zebra_chain::parameters::{
        subsidy::{funding_stream_address_period, halving, ParameterSubsidy},
        testnet::{ConfiguredFundingStreams, RegtestParameters},
    };

    // Regtest address periods are 6 blocks and, with NU7 at an even height, change at heights
    // 5, 11, 17, ...; activation at 14 lands inside 11..=16, which ZIP 207 stretches to 11..=22.
    const NU7: u32 = 14;
    const STRETCHED_BOUNDARY: u32 = 23;
    const STREAM_END: u32 = 40;

    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(300), async {
        let fs_address = |byte: u8| {
            transparent::Address::from_script_hash(NetworkKind::Testnet, [byte; 20]).to_string()
        };
        let network = Network::new_regtest(RegtestParameters {
            activation_heights: ConfiguredActivationHeights {
                canopy: Some(1),
                nu5: Some(2),
                nu6: Some(4),
                nu6_1: Some(5),
                nu6_2: Some(6),
                nu6_3: Some(7),
                nu7: Some(NU7),
                ..Default::default()
            },
            funding_streams: Some(vec![ConfiguredFundingStreams {
                // Starts at NU6 so the lockbox stream applies throughout, and ends after NU7.
                height_range: Some(Height(4)..Height(STREAM_END + 1)),
                recipients: Some(vec![
                    stream_recipient(FundingStreamReceiver::Deferred, 12, None),
                    stream_recipient(
                        FundingStreamReceiver::MajorGrants,
                        8,
                        Some(vec![fs_address(1), fs_address(2)]),
                    ),
                    stream_recipient(
                        FundingStreamReceiver::ZcashFoundation,
                        5,
                        Some(vec![fs_address(3), fs_address(4)]),
                    ),
                ]),
            }]),
            // Two addresses per recipient cycle over the four address periods in the range.
            extend_funding_stream_addresses_as_required: Some(true),
            ..Default::default()
        });
        // The hard-coded heights above follow from the pre-NU7 6-block period and ZIP 207.
        let period = |height: u32| funding_stream_address_period(Height(height), &network);
        assert_eq!(network.funding_stream_address_change_interval(), 6);
        assert_eq!(period(11), period(10) + 1);
        assert_eq!(period(11), period(STRETCHED_BOUNDARY - 1));
        assert_eq!(period(STRETCHED_BOUNDARY), period(11) + 1);

        let miner = transparent::Address::from_pub_key_hash(NetworkKind::Testnet, [0xda; 20]);
        let mut config = os_assigned_rpc_port_config(false, &network)?;
        config.mining.miner_address = Some(miner.to_string().parse()?);
        let mut child = testdir()?
            .with_config(&mut config)?
            .spawn_child(args!["start"])?;
        let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;
        tokio::time::sleep(LAUNCH_DELAY).await;
        let client = RpcRequestClient::new(rpc_address);

        let branch = |upgrade: NetworkUpgrade| {
            hex::encode(u32::from(upgrade.branch_id().expect("has a branch id")).to_be_bytes())
        };
        // Before NSM reissuance the subsidy is a pure function of height, so querying the
        // boundary heights from the genesis tip must match the queries made at their parents.
        let mut early = Vec::new();
        for height in [NU7 - 1, NU7] {
            early.push(checked_subsidy(&client, &network, height).await?);
        }
        let mut subsidies = Vec::new();
        let mut lockbox_pools = Vec::new();
        for height in 1..=STREAM_END + 1 {
            let response = checked_subsidy(&client, &network, height).await?;
            if height == NU7 - 1 || height == NU7 {
                assert_eq!(response, early[(height + 1 - NU7) as usize]);
            }
            let (block, template_height) = client.block_from_template(&network).await?;
            assert_eq!(template_height, Height(height));

            if height == NU7 {
                // A coinbase paying the pre-NU7 amounts (three times the NU7 subsidy) is invalid.
                let mut outputs = block.transactions[0].outputs();
                for output in &mut outputs {
                    output.value = (output.value * 3)?;
                }
                let overpaid = with_coinbase_outputs(&block, outputs);
                assert!(!proposal(&client, &overpaid).await?.is_valid());
            }
            if height == STRETCHED_BOUNDARY {
                // Paying the previous address period's address after the boundary is invalid.
                let previous: &GetBlockSubsidyResponse = &subsidies[height as usize - 2];
                let [stale, current] = [previous, &response].map(|subsidy| {
                    let stream = &subsidy.funding_streams()[0];
                    (&stream.recipient, stream.address.expect("transparent"))
                });
                assert_eq!(stale.0, current.0);
                assert_ne!(stale.1, current.1);
                let mut outputs = block.transactions[0].outputs();
                for output in outputs
                    .iter_mut()
                    .filter(|o| o.lock_script == current.1.script())
                {
                    output.lock_script = stale.1.script();
                }
                let stale_address = with_coinbase_outputs(&block, outputs);
                assert!(!proposal(&client, &stale_address).await?.is_valid());
            }

            let (hash, coinbase_id) = (block.hash(), block.transactions[0].hash());
            client.submit_block(block).await?;
            if height == NU7 {
                let verbose: serde_json::Value = client
                    .json_result_from_call("getblock", format!(r#"["{height}", 1]"#))
                    .await
                    .map_err(|err| eyre!(err))?;
                assert_eq!(verbose["hash"], hash.to_string());
                assert_eq!(verbose["height"], NU7);
                assert_eq!(verbose["confirmations"], 1);
                assert_eq!(verbose["tx"][0], coinbase_id.to_string());
            }
            checked_coinbase(&client, &miner, height, &response, &[]).await?;

            if height == NU7 - 1 || height == NU7 {
                let info = chain_info(&client).await?;
                let nu7 = &info["upgrades"][branch(NetworkUpgrade::Nu7)];
                assert_eq!(nu7["activationheight"], NU7);
                let (status, chain_tip) = if height == NU7 {
                    ("active", NetworkUpgrade::Nu7)
                } else {
                    ("pending", NetworkUpgrade::Nu6_3)
                };
                assert_eq!(nu7["status"], status);
                assert_eq!(info["consensus"]["chaintip"], branch(chain_tip));
                assert_eq!(info["consensus"]["nextblock"], branch(NetworkUpgrade::Nu7));
            }
            lockbox_pools.push(lockbox_pool(&client).await?);
            subsidies.push(response);
        }

        let at = |height: u32| &subsidies[height as usize - 1];
        let pool = |height: u32| lockbox_pools[height as usize - 1];
        let total = |height: u32| zat(at(height).total_block_subsidy());
        let streams =
            |height: u32| (at(height).funding_streams().iter()).chain(at(height).lockbox_streams());

        // ZIP 218: within the same halving, the NU7 subsidy is exactly one third of the pre-NU7
        // subsidy; each stream share is floored after that division, so it may lose one zatoshi.
        assert_eq!(
            halving(Height(NU7 - 1), &network),
            halving(Height(NU7), &network)
        );
        assert_eq!(total(NU7), total(NU7 - 1) / 3);
        assert_eq!(streams(NU7).count(), 3);
        for (before, after) in streams(NU7 - 1).zip(streams(NU7)) {
            assert_eq!(before.recipient, after.recipient);
            let (third, post) = (u64::from(before.value_zat) / 3, u64::from(after.value_zat));
            assert!(post <= third && third - post <= 1, "{third} vs {post}");
        }
        assert_eq!(
            pool(NU7 - 1) - pool(NU7 - 2),
            zat(at(NU7 - 1).lockbox_total())
        );
        assert_eq!(pool(NU7) - pool(NU7 - 1), zat(at(NU7).lockbox_total()));
        assert!(zat(at(NU7).lockbox_total()) > 0);

        // ZIP 207: the address period containing activation is stretched, so the recipients keep
        // their addresses at the old 6-block boundary (17) and change at the stretched one (23).
        for index in 0..2 {
            let address = |height: u32| at(height).funding_streams()[index].address;
            assert_ne!(address(10), address(11));
            for height in [NU7 - 1, NU7, 17, STRETCHED_BOUNDARY - 1] {
                assert_eq!(address(height), address(11), "address at {height}");
            }
            assert_ne!(address(STRETCHED_BOUNDARY - 1), address(STRETCHED_BOUNDARY));
            assert_eq!(address(STRETCHED_BOUNDARY), address(STREAM_END));
        }

        // The streams stop after the configured end height; the miner then takes the whole subsidy.
        assert_eq!(streams(STREAM_END).count(), 3);
        assert_eq!(streams(STREAM_END + 1).count(), 0);
        assert_eq!(zat(at(STREAM_END + 1).miner()), total(STREAM_END + 1));

        child.kill(false)?;
        let output = child.wait_with_output()?;
        output.assert_was_killed()?;
        output.assert_failure()?;
        Ok(())
    })
    .await?
}

/// ZIP 2003 and ZIP 218 block rules at NU7 activation, and coinbase maturity across it.
#[tokio::test(flavor = "multi_thread")]
async fn nu7_block_rules_at_activation() -> Result<()> {
    use zebra_chain::{
        parameters::testnet::RegtestParameters,
        transaction::arbitrary::{
            fake_orchard_bundle, fake_v6_transaction, insert_fake_orchard_shielded_data,
        },
    };
    use zebra_rpc::server::error::LegacyCode;

    const NU7: u32 = 105;
    // `OrchardProtocolBlockActionLimit` in ZIP 218, pinned here rather than imported.
    const ORCHARD_BLOCK_ACTION_LIMIT: usize = 330;

    /// A V6 transaction with `actions` dummy Orchard actions: the ZIP 218 limits are counted
    /// before any proof or state check, so no valid proofs are needed.
    fn orchard_actions(actions: usize, seed: u64) -> Transaction {
        let one =
            insert_fake_orchard_shielded_data(fake_v6_transaction(NetworkUpgrade::Nu7, None, None));
        let bundle = one.orchard_bundle().expect("inserted above");
        let bundle = fake_orchard_bundle(
            *bundle.flags(),
            *bundle.value_balance(),
            actions,
            seed,
            bundle.bundle_version(),
        );
        one.with_orchard_bundle(Some(bundle))
    }

    fn with_transactions(block: &Block, extra: Vec<Transaction>) -> Block {
        let mut block = block.clone();
        block.transactions.extend(extra.into_iter().map(Arc::new));
        Arc::make_mut(&mut block.header).merkle_root = block.transactions.iter().collect();
        block
    }

    /// The `sendrawtransaction` error code and message for a transaction the mempool rejects.
    async fn send_error(client: &RpcRequestClient, tx: &Transaction) -> Result<(i64, String)> {
        let data = hex::encode(tx.zcash_serialize_to_vec()?);
        let response: serde_json::Value = serde_json::from_str(
            &client
                .text_from_call("sendrawtransaction", format!(r#"["{data}"]"#))
                .await?,
        )?;
        let code = response["error"]["code"].as_i64();
        let message = response["error"]["message"].as_str().unwrap_or_default();
        Ok((
            code.ok_or_else(|| eyre!("accepted: {response}"))?,
            message.to_string(),
        ))
    }

    async fn mine(client: &RpcRequestClient, network: &Network, height: u32) -> Result<Block> {
        let (block, template_height) = client.block_from_template(network).await?;
        assert_eq!(template_height, Height(height));
        client.submit_block(block.clone()).await?;
        Ok(block)
    }

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
                nu7: Some(NU7),
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
        let verify_code = i64::from(i32::from(LegacyCode::Verify));

        let mature_coinbase = mine(&client, &network, 1).await?.transactions[0].clone();
        for height in 2..NU7 {
            mine(&client, &network, height).await?;
        }
        let (activation, height) = client.block_from_template(&network).await?;
        assert_eq!(height, Height(NU7));

        // ZIP 2003: a V4 transaction is rejected in the activation block, before its inputs matter.
        let v4 = spend(&mature_coinbase, false, 10_001)?;
        let reason = rejected(&client, &with_transactions(&activation, vec![v4])).await?;
        assert!(
            reason.contains("unsupportedbynetworkupgrade-4-nu7"),
            "{reason}"
        );

        // ZIP 218, mempool side: one transaction over the Orchard action limit for the next block.
        let over = orchard_actions(ORCHARD_BLOCK_ACTION_LIMIT + 1, 1);
        let (code, message) = send_error(&client, &over).await?;
        assert_eq!(code, verify_code, "{message}");
        let expected = format!("{} Orchard actions", ORCHARD_BLOCK_ACTION_LIMIT + 1);
        assert!(message.contains(&expected), "{message}");
        mempool(&client, &[]).await?;

        // ZIP 218, block side: two transactions within the limit exceed it together.
        let half = ORCHARD_BLOCK_ACTION_LIMIT / 2 + 1;
        let pair = vec![orchard_actions(half, 2), orchard_actions(half, 3)];
        let reason = rejected(&client, &with_transactions(&activation, pair)).await?;
        assert!(reason.contains("toomanyshieldedactions"), "{reason}");
        client.submit_block(activation).await?;

        // MIN_TRANSPARENT_COINBASE_MATURITY (100 blocks) is unchanged by NU7: a coinbase mined
        // at NU7 + 1 is spendable from NU7 + 101, not NU7 + 100.
        let post_nu7_coinbase = mine(&client, &network, NU7 + 1).await?.transactions[0].clone();
        for height in NU7 + 2..NU7 + 100 {
            mine(&client, &network, height).await?;
        }
        let early = spend_expiring(&post_nu7_coinbase, true, 10_001, Height(0))?;
        let (code, message) = send_error(&client, &early).await?;
        assert_eq!(code, verify_code, "{message}");
        assert!(
            message.contains("immature transparent coinbase spend"),
            "{message}"
        );
        mempool(&client, &[]).await?;
        mine(&client, &network, NU7 + 100).await?;
        let mature = spend_expiring(&post_nu7_coinbase, true, 10_002, Height(0))?;
        send(&client, &mature).await?;
        mempool(&client, &[mature.hash()]).await?;
        let block = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let (block, _) = client.block_from_template(&network).await?;
                if block
                    .transactions
                    .iter()
                    .any(|tx| tx.hash() == mature.hash())
                {
                    return Ok::<_, color_eyre::Report>(block);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await??;
        assert_eq!(block.coinbase_height(), Some(Height(NU7 + 101)));
        client.submit_block(block).await?;
        mempool(&client, &[]).await?;

        child.kill(false)?;
        let output = child.wait_with_output()?;
        output.assert_was_killed()?;
        output.assert_failure()?;
        Ok(())
    })
    .await?
}

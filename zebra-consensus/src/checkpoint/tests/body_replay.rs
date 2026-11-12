//! Checkpoint-certified body replacement without replaying accepted descendants.

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn targeted_body_replay_preserves_in_flight_descendants() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    let genesis =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
    let blocks = [
        &zebra_test::vectors::BLOCK_MAINNET_1687106_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_1687107_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES[..],
    ]
    .map(Arc::<Block>::zcash_deserialize);
    let [first, replacement, descendant] = blocks;
    let (first, replacement, descendant) = (first?, replacement?, descendant?);
    let mut state = MockService::build().for_unit_tests();
    let mut verifier = CheckpointVerifier::from_list(
        [
            (block::Height(0), genesis.hash()),
            (block::Height(1_687_108), descendant.hash()),
        ],
        &Mainnet,
        Some((block::Height(1_687_105), first.header.previous_block_hash)),
        state.clone(),
    )
    .map_err(|error| eyre!(error))?;

    let first_result = verifier.call(first.clone());
    let rejected_result = verifier.call(replacement.clone());
    let descendant_result = verifier.call(descendant.clone());
    state
        .expect_request_that(|request| {
            matches!(request, zs::Request::CommitCheckpointVerifiedBlock(block)
                if block.hash == first.hash())
        })
        .await
        .respond(zs::Response::Committed(first.hash()));
    assert_eq!(
        timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), first_result).await??,
        first.hash()
    );
    let rejected = state
        .expect_request_that(|request| {
            matches!(request, zs::Request::CommitCheckpointVerifiedBlock(block)
                if block.hash == replacement.hash())
        })
        .await;
    let pending_descendant = state
        .expect_request_that(|request| {
            matches!(request, zs::Request::CommitCheckpointVerifiedBlock(block)
                if block.hash == descendant.hash())
        })
        .await;
    rejected.respond_error("authorizing body needs replacement".into());
    state
        .expect_request_that(|request| matches!(request, zs::Request::Tip))
        .await
        .respond(zs::Response::Tip(Some((
            block::Height(1_687_106),
            first.hash(),
        ))));
    assert!(
        timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), rejected_result)
            .await?
            .is_err()
    );

    // Inject the writer's targeted control event. Its real-state regression checks the flag;
    // this verifier boundary must recheck effects without requiring the in-flight endpoint again.
    let failure = verifier.reset_receiver.try_recv()?;
    // The accepted descendant and the rejected body's reserved replay slot fill admission.
    verifier.retained_block_limit = 2;
    verifier.retained_blocks.fetch_add(1, Ordering::Relaxed);
    let retained = RetainedBlockPermit(verifier.retained_blocks.clone());
    verifier.reset_sender.send(CommitFailure {
        epoch: failure.epoch,
        tip: None,
        child: None,
        body_replay: Some(BodyReplay {
            height: block::Height(1_687_107),
            hash: replacement.hash(),
            retained,
        }),
    })?;

    let mut altered_effects = (*replacement).clone();
    altered_effects
        .transactions
        .push(altered_effects.transactions[0].clone());
    assert_eq!(altered_effects.hash(), replacement.hash());
    let error = verifier
        .call(Arc::new(altered_effects))
        .await
        .expect_err("checkpoint identity does not authenticate a replacement's effects");
    assert_eq!(error.misbehavior_score(), 100);

    let replay_result = verifier.call(replacement.clone());
    state
        .expect_request_that(|request| {
            matches!(request, zs::Request::CommitCheckpointVerifiedBlock(block)
                if block.hash == replacement.hash())
        })
        .await
        .respond(zs::Response::Committed(replacement.hash()));
    pending_descendant.respond(zs::Response::Committed(descendant.hash()));

    for (result, expected) in [
        (replay_result, replacement.hash()),
        (descendant_result, descendant.hash()),
    ] {
        assert_eq!(
            timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), result).await??,
            expected
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn targeted_body_replay_rejects_changed_coinbase_height() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    let genesis =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
    let replacement =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1687107_BYTES[..])?;
    let descendant =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES[..])?;
    let height = replacement.coinbase_height().unwrap();
    let mut state = MockService::build().for_unit_tests();
    let mut verifier = CheckpointVerifier::from_list(
        [
            (block::Height(0), genesis.hash()),
            (descendant.coinbase_height().unwrap(), descendant.hash()),
        ],
        &Mainnet,
        Some((
            block::Height(height.0 - 1),
            replacement.header.previous_block_hash,
        )),
        state.clone(),
    )
    .map_err(|error| eyre!(error))?;
    verifier.retained_blocks.fetch_add(1, Ordering::Relaxed);
    verifier.body_replay = Some(BodyReplay {
        height,
        hash: replacement.hash(),
        retained: RetainedBlockPermit(verifier.retained_blocks.clone()),
    });

    let mut forged = (*replacement).clone();
    let mut inputs = forged.transactions[0].inputs();
    let zebra_chain::transparent::Input::Coinbase {
        height: changed, ..
    } = &mut inputs[0]
    else {
        panic!("the fixture starts with a coinbase input");
    };
    *changed = block::Height(height.0 + 1);
    forged.transactions[0] = Arc::new(
        (*forged.transactions[0])
            .clone()
            .with_transparent_inputs(inputs),
    );
    assert_eq!(forged.transactions[0].version(), 5);
    assert_eq!(forged.hash(), replacement.hash());
    assert_eq!(
        forged.transactions[0].hash(),
        replacement.transactions[0].hash()
    );
    let rejected = verifier.call(Arc::new(forged));
    assert_eq!(
        verifier.body_replay.as_ref().map(|replay| replay.height),
        Some(height),
        "a wrong height must not consume the certified replay slot",
    );
    let error = timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), rejected)
        .await?
        .unwrap_err();
    assert!(matches!(
        error,
        VerifyCheckpointError::CoinbaseHeight { .. }
    ));
    assert_eq!(error.misbehavior_score(), 100);

    let replay = verifier.call(replacement.clone());
    state
        .expect_request_that(|request| {
            matches!(request, zs::Request::CommitCheckpointVerifiedBlock(block)
                if block.hash == replacement.hash() && block.height == height)
        })
        .await
        .respond(zs::Response::Committed(replacement.hash()));
    assert_eq!(
        timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), replay).await??,
        replacement.hash(),
    );
    Ok(())
}

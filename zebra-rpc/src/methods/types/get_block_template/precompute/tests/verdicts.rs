//! Failed generations are withdrawn without discarding newer work or an independent valid fallback.

use super::*;

#[test]
fn rejected_work_without_a_valid_fallback_is_withdrawn() {
    let cache = TemplateCache::default();
    cache.publish(full_template(0));
    let failed = served_work(&cache).unwrap();
    let mut changes = cache.subscribe();

    assert!(cache.reject(&failed));
    assert!(changes.take_changed());
    assert!(served_work(&cache).is_none());
    assert!(next_work(&cache).is_none());
    assert!(cache
        .template_for_tip(
            failed.previous_block_hash,
            &Network::Mainnet,
            failed.cur_time
        )
        .is_none());
}

#[test]
fn older_rejection_preserves_the_newer_served_generation_and_queued_check() {
    let cache = TemplateCache::default();
    cache.publish(template());
    cache.publish(full_template(0));
    let older = served_work(&cache).unwrap();
    cache.publish(full_template(1));
    let newer = served_work(&cache).unwrap();
    let mut changes = cache.subscribe();

    assert!(cache.reject(&older));
    assert!(changes.take_changed());
    assert!(served_work(&cache).is_some_and(|served| Arc::ptr_eq(&served, &newer)));
    assert!(next_work(&cache).is_some_and(|next| Arc::ptr_eq(&next, &newer)));
    assert!(!cache.reject(&older));
    assert!(!changes.take_changed());
}

#[test]
fn late_success_does_not_replace_a_newer_valid_fallback() {
    let cache = TemplateCache::default();
    cache.publish(full_template(0));
    let older = served_work(&cache).unwrap();
    cache.publish(full_template(1));
    let valid = served_work(&cache).unwrap();
    assert!(cache.accept_validated(&valid, Some(&valid)));
    cache.publish(full_template(2));
    let failed = served_work(&cache).unwrap();

    assert!(!cache.accept_validated(&older, Some(&older)));
    assert!(cache.reject(&failed));
    assert!(served_work(&cache).is_some_and(|served| Arc::ptr_eq(&served, &valid)));
}

#[test]
fn rejected_clock_narrowing_withdraws_its_source_generation() {
    for source_is_valid in [false, true] {
        let network = Network::Mainnet;
        let cache = TemplateCache::default();
        cache.publish(template());
        let coinbase_only = served_work(&cache).unwrap();
        cache.publish(full_template(0));
        let source = served_work(&cache).unwrap();
        if source_is_valid {
            assert!(cache.accept_validated(&source, Some(&source)));
        }
        let now = source
            .cur_time
            .saturating_sub(Duration32::from_hours(2))
            .saturating_sub(Duration32::from_seconds(1));
        let narrowed = cache
            .template_for_tip(source.previous_block_hash, &network, now)
            .unwrap();
        assert_ne!(narrowed.cur_time, source.cur_time);
        assert!(!Arc::ptr_eq(&source, &narrowed));
        let mut changes = cache.subscribe();

        assert!(cache.reject(&narrowed));
        assert!(changes.take_changed());
        assert!(next_work(&cache).is_none());
        if source_is_valid {
            assert!(cache
                .template_for_tip(source.previous_block_hash, &network, now)
                .is_none());
        } else {
            assert!(served_work(&cache).is_some_and(|served| Arc::ptr_eq(&served, &coinbase_only)));
            assert!(cache
                .template_for_tip(source.previous_block_hash, &network, now)
                .is_some_and(|served| served.transactions.is_empty()));
        }
    }
}

#[test]
fn rejected_narrowed_fallback_is_not_restored_after_a_later_rejection() {
    let network = Network::Mainnet;
    let cache = TemplateCache::default();
    cache.publish(full_template(0));
    let source = served_work(&cache).unwrap();
    assert!(cache.accept_validated(&source, Some(&source)));
    let now = source
        .cur_time
        .saturating_sub(Duration32::from_hours(2))
        .saturating_sub(Duration32::from_seconds(1));
    let narrowed = cache
        .template_for_tip(source.previous_block_hash, &network, now)
        .unwrap();
    cache.publish(full_template(1));
    let newer = served_work(&cache).unwrap();
    let mut changes = cache.subscribe();

    assert!(cache.reject(&narrowed));
    assert!(changes.take_changed());
    assert!(served_work(&cache).is_some_and(|served| Arc::ptr_eq(&served, &newer)));
    assert!(next_work(&cache).is_some_and(|next| Arc::ptr_eq(&next, &newer)));
    assert!(cache.reject(&newer));
    assert!(changes.take_changed());
    assert!(cache
        .template_for_tip(source.previous_block_hash, &network, now)
        .is_none());
}

#[tokio::test(start_paused = true)]
async fn withdrawal_interrupts_membership_waits_without_bypassing_valid_fallback_pacing() {
    for (has_fallback, during_check) in [(false, false), (false, true), (true, false), (true, true)] {
        let cache = TemplateCache::default();
        let fallback = has_fallback.then(|| {
            cache.publish(template());
            served_work(&cache).unwrap()
        });
        cache.publish(full_template(0));
        let failed = served_work(&cache).unwrap();
        let reading = Arc::new(AtomicBool::new(false));
        let mempool = tower::service_fn({
            let reading = reading.clone();
            move |request| {
                assert_eq!(request, mempool::Request::TransactionIds);
                reading.store(true, Ordering::SeqCst);
                std::future::pending::<Result<mempool::Response, zebra_node_services::BoxError>>()
            }
        });
        let (events, mut receiver) = broadcast::channel(1);
        let ids = HashSet::new();
        let mut publications = cache.subscribe();
        let started = Instant::now();
        let waiting = async {
            tokio::select! {
                _ = publications.withdrawn() => {}
                _ = wait_for_mempool_change(
                    &mut receiver,
                    mempool,
                    &ids,
                    started + MEMPOOL_SHORT_PAUSE,
                    started + MEMPOOL_THROTTLE,
                ) => {}
            }
        };
        tokio::pin!(waiting);
        assert!(futures::poll!(&mut waiting).is_pending());
        if during_check {
            let rejected =
                UnminedTxId::from_legacy_id(zebra_chain::transaction::Hash([9; 32]));
            events
                .send(MempoolChange::invalidated([rejected].into_iter().collect()))
                .unwrap();
            assert!(futures::poll!(&mut waiting).is_pending());
            tokio::time::advance(MEMPOOL_SHORT_PAUSE).await;
            assert!(futures::poll!(&mut waiting).is_pending());
            assert!(reading.load(Ordering::SeqCst));
        }
        let rejected_at = Instant::now();
        assert!(cache.reject(&failed));
        if let Some(fallback) = fallback {
            assert!(futures::poll!(&mut waiting).is_pending());
            assert!(served_work(&cache).is_some_and(|served| Arc::ptr_eq(&served, &fallback)));
            cache.clear();
        }
        assert!(!cache.is_uninitialized());
        assert!(futures::poll!(&mut waiting).is_ready());
        assert_eq!(rejected_at.elapsed(), Duration::ZERO);
    }
}

#[tokio::test]
async fn fallback_rejection_reaches_the_cache_across_updater_handoff() {
    use zebra_test::mock_service::MockService;

    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(10), async {
        for has_valid_fallback in [false, true] {
            let cache = TemplateCache::default();
            let fallback = has_valid_fallback.then(|| {
                cache.publish(template());
                served_work(&cache).unwrap()
            });
            let mut verifier: MockService<
                VerifyRequest,
                block::Hash,
                _,
                zebra_consensus::BoxError,
            > = MockService::build()
                .with_max_request_delay(Duration::from_secs(10))
                .for_unit_tests();
            cache.publish_fallback(full_template(0), Network::Mainnet, verifier.clone());
            let failed = served_work(&cache).unwrap();
            let request = verifier
                .expect_request_that(|request| matches!(request, VerifyRequest::CheckProposal(_)))
                .await;
            assert_eq!(
                *request.request().block(),
                proposal_block_from_template(
                    &failed,
                    BlockTemplateTimeSource::CurTime,
                    &Network::Mainnet,
                )
                .unwrap()
            );
            let _updater = cache.claim_updater().unwrap();
            let mut changes = cache.subscribe();
            request.respond_error("rejected".into());
            changes.changed().await;
            assert_eq!(
                served_work(&cache).map(|served| served.generation),
                fallback.map(|valid| valid.generation),
            );
            assert!(next_work(&cache).is_none());
        }
    })
    .await
    .expect("fallback validation must withdraw rejected work after updater handoff");
}

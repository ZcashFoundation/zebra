use super::{config::*, error::*, events::*, scheduler::*, state::*, validation::*, wire::*, *};

/// Spawn a header-sync reactor and return its handle plus action stream.
pub fn spawn_header_sync_reactor(
    startup: HeaderSyncStartup,
) -> Result<
    (
        HeaderSyncHandle,
        mpsc::Receiver<HeaderSyncAction>,
        JoinHandle<()>,
    ),
    HeaderSyncStartError,
> {
    let state = HeaderSyncState::new(&startup)?;
    let (events_tx, events_rx) = mpsc::channel(128);
    let (lifecycle_tx, lifecycle_rx) = mpsc::unbounded_channel();
    let (actions_tx, actions_rx) = mpsc::channel(128);
    let (tip_tx, tip_rx) = watch::channel((state.best_header_tip, state.best_header_hash));
    let handle = HeaderSyncHandle {
        events: events_tx,
        lifecycle: lifecycle_tx,
        tip: tip_rx,
    };
    let reactor = HeaderSyncReactor {
        startup,
        state,
        events: events_rx,
        lifecycle: lifecycle_rx,
        actions: actions_tx,
        tip: tip_tx,
    };
    let task = tokio::spawn(reactor.run());

    Ok((handle, actions_rx, task))
}

#[derive(Debug)]
pub(super) struct HeaderSyncReactor {
    startup: HeaderSyncStartup,
    state: HeaderSyncState,
    events: mpsc::Receiver<HeaderSyncEvent>,
    lifecycle: mpsc::UnboundedReceiver<HeaderSyncEvent>,
    actions: mpsc::Sender<HeaderSyncAction>,
    tip: watch::Sender<(block::Height, block::Hash)>,
}

impl HeaderSyncReactor {
    async fn run(mut self) {
        if self.startup.range_state_actions_enabled {
            let _ = self
                .actions
                .send(HeaderSyncAction::QueryBestHeaderTip)
                .await;
            let _ = self
                .actions
                .send(HeaderSyncAction::QueryMissingBlockBodies {
                    from: next_height(self.state.verified_block_tip)
                        .unwrap_or(self.state.verified_block_tip),
                    limit: DEFAULT_HS_RANGE,
                })
                .await;
        }

        let mut ticks = time::interval(self.empty_headers_retry_delay());
        loop {
            tokio::select! {
                biased;
                _ = self.startup.shutdown.cancelled() => {
                    break;
                }
                event = self.lifecycle.recv() => {
                    let Some(event) = event else {
                        break;
                    };
                    self.handle_event(event).await;
                }
                event = self.events.recv() => {
                    let Some(event) = event else {
                        break;
                    };
                    self.handle_event(event).await;
                }
                _ = ticks.tick() => {
                    self.handle_timeouts().await;
                }
            }
        }
    }

    async fn handle_event(&mut self, event: HeaderSyncEvent) {
        match event {
            HeaderSyncEvent::PeerConnected(peer) => self.handle_peer_connected(peer).await,
            HeaderSyncEvent::PeerDisconnected(peer) => self.handle_peer_disconnected(peer),
            HeaderSyncEvent::FullBlockCommitted {
                height,
                hash,
                header: _,
            } => self.handle_full_block_committed(height, hash).await,
            HeaderSyncEvent::NewBlockAccepted {
                peer,
                height,
                hash,
                block,
            } => {
                self.handle_new_block_accepted(peer, height, hash, block)
                    .await
            }
            HeaderSyncEvent::NewBlockDuplicate { peer, height, hash } => {
                self.handle_new_block_duplicate(peer, height, hash)
            }
            HeaderSyncEvent::NewBlockRejected { peer, hash } => {
                self.handle_new_block_rejected(peer, hash).await
            }
            HeaderSyncEvent::WireMessage { peer, msg } => {
                self.handle_wire_message(peer, msg).await;
            }
            HeaderSyncEvent::WireFrame { peer, frame } => {
                self.handle_wire_frame(peer, frame).await;
            }
            HeaderSyncEvent::WireDecodeFailed { peer, error } => {
                self.handle_wire_decode_failed(peer, error).await;
            }
            HeaderSyncEvent::StateFrontiersChanged(frontiers) => {
                self.handle_state_frontiers_changed(frontiers).await;
            }
            HeaderSyncEvent::HeaderRangeCommitted {
                start_height,
                tip_height,
                tip_hash,
            } => {
                self.handle_header_range_committed(start_height, tip_height, tip_hash)
                    .await
            }
            HeaderSyncEvent::HeaderRangeCommitFailed {
                peer,
                start_height,
                count,
                kind,
            } => {
                self.handle_header_range_commit_failed(peer, start_height, count, kind)
                    .await
            }
            HeaderSyncEvent::HeaderRangeResponseFinished {
                peer,
                start_height,
                requested_count,
                returned_count,
            } => self.handle_header_range_response_finished(
                peer,
                start_height,
                requested_count,
                returned_count,
            ),
        }
    }

    async fn handle_peer_connected(&mut self, peer: ZakuraPeerId) {
        self.state.peers.entry(peer.clone()).or_insert_with(|| {
            PeerHeaderState::new(
                self.state.anchor.0,
                self.startup.config.advertised_max_headers_per_response(),
                self.startup.config.advertised_max_inflight_requests(),
                self.startup.status_refresh_interval,
                DEFAULT_HS_INBOUND_STATUS_MIN_INTERVAL,
                DEFAULT_HS_INBOUND_NEW_BLOCK_MIN_INTERVAL,
            )
        });
        self.send_status(peer).await;
        self.schedule().await;
    }

    fn handle_peer_disconnected(&mut self, peer: ZakuraPeerId) {
        self.state.peers.remove(&peer);
        self.state.schedule.forget_peer(&peer);
    }

    async fn handle_full_block_committed(&mut self, height: block::Height, hash: block::Hash) {
        self.state.pending_new_blocks.remove(&hash);
        let _ = self.state.seen.insert(hash);
        self.state.verified_block_tip = self.state.verified_block_tip.max(height);
        self.state.schedule.mark_height_covered(height);
        self.cancel_covered_outstanding();
        if height > self.state.best_header_tip {
            self.publish_best_tip(height, hash).await;
        }
        self.schedule().await;
    }

    async fn handle_new_block_accepted(
        &mut self,
        peer: ZakuraPeerId,
        height: block::Height,
        hash: block::Hash,
        block: Arc<block::Block>,
    ) {
        self.state.pending_new_blocks.remove(&hash);
        let inserted = self.state.seen.insert(hash);
        if !inserted {
            metrics::counter!("sync.header.tip.new_block.deduped").increment(1);
            self.trace_new_block_deduped(&peer, height, hash, "seen_cache");
            return;
        }

        self.state.verified_block_tip = self.state.verified_block_tip.max(height);
        self.state.schedule.mark_height_covered(height);
        self.cancel_covered_outstanding();
        if height > self.state.best_header_tip {
            self.publish_best_tip(height, hash).await;
        }

        let destinations = self.eligible_tip_destinations(&peer, height);
        let destination_count = destinations.len();
        for destination in destinations {
            metrics::counter!("sync.header.tip.new_block.forwarded").increment(1);
            self.trace_new_block_forwarded(&peer, &destination, height, hash, destination_count);
            let _ = self
                .actions
                .send(HeaderSyncAction::ForwardNewBlock {
                    source: Some(peer.clone()),
                    peer: destination,
                    height,
                    hash,
                    block: block.clone(),
                })
                .await;
        }
        self.schedule().await;
    }

    fn handle_new_block_duplicate(
        &mut self,
        peer: ZakuraPeerId,
        height: block::Height,
        hash: block::Hash,
    ) {
        self.state.pending_new_blocks.remove(&hash);
        let _ = self.state.seen.insert(hash);
        metrics::counter!("sync.header.tip.new_block.deduped").increment(1);
        self.trace_new_block_deduped(&peer, height, hash, "already_in_chain");
    }

    async fn handle_new_block_rejected(&mut self, peer: ZakuraPeerId, hash: block::Hash) {
        self.state.pending_new_blocks.remove(&hash);
        metrics::counter!("sync.header.tip.new_block.rejected").increment(1);
        debug!(
            ?peer,
            ?hash,
            "Zakura header-sync NewBlock rejected by block pipeline"
        );
        self.report_misbehavior(peer, HeaderSyncMisbehavior::InvalidNewBlock)
            .await;
    }

    async fn handle_wire_decode_failed(
        &mut self,
        peer: ZakuraPeerId,
        error: Arc<HeaderSyncWireError>,
    ) {
        self.trace_peer_violation(&peer, HeaderSyncMisbehavior::MalformedMessage);
        tracing::debug!(?peer, ?error, "malformed Zakura header-sync frame");
        self.report_misbehavior(peer, HeaderSyncMisbehavior::MalformedMessage)
            .await;
    }

    async fn handle_state_frontiers_changed(&mut self, frontiers: HeaderSyncFrontiers) {
        self.state.finalized_height = frontiers.finalized_height;
        self.state.verified_block_tip = frontiers.verified_block_tip;
        self.schedule().await;
    }

    async fn handle_header_range_committed(
        &mut self,
        start_height: block::Height,
        tip_height: block::Height,
        tip_hash: block::Hash,
    ) {
        metrics::counter!("sync.header.range.committed").increment(1);
        self.trace_range_event(
            hs_trace::HEADER_RANGE_COMMITTED,
            start_height,
            count_between(start_height, tip_height),
            None,
            None,
        );
        self.state
            .pending_commits
            .retain(|_, range| !range.is_within(start_height, tip_height));
        self.state
            .schedule
            .mark_range_covered(start_height, tip_height);
        // The zebrad driver also uses this event to reload the durable best header tip at
        // startup. In that path start==tip, so covered-range side effects are bounded.
        self.cancel_covered_outstanding();
        if tip_height > self.state.best_header_tip {
            self.publish_best_tip(tip_height, tip_hash).await;
        }
        self.notify_body_gaps().await;
        self.schedule().await;
    }

    async fn handle_header_range_commit_failed(
        &mut self,
        peer: ZakuraPeerId,
        start_height: block::Height,
        count: u32,
        kind: HeaderSyncCommitFailureKind,
    ) {
        metrics::counter!("sync.header.range.rejected").increment(1);
        self.trace_range_event(
            hs_trace::HEADER_RANGE_REJECTED,
            start_height,
            count,
            Some(&peer),
            Some(commit_failure_reason_label(kind)),
        );
        if kind == HeaderSyncCommitFailureKind::InvalidPeerRange {
            self.report_misbehavior(peer.clone(), HeaderSyncMisbehavior::InvalidRange)
                .await;
        }
        let key = PendingCommitKey {
            peer,
            start_height,
            count,
        };
        if let Some(range) = self.state.pending_commits.remove(&key) {
            if kind == HeaderSyncCommitFailureKind::Local {
                self.state.schedule.clear_assignment(range);
            }
            self.state.schedule.retry(range);
        }
        self.schedule().await;
    }

    fn handle_header_range_response_finished(
        &mut self,
        peer: ZakuraPeerId,
        start_height: block::Height,
        requested_count: u32,
        returned_count: u32,
    ) {
        self.trace_headers_served(&peer, start_height, requested_count, returned_count);
        if let Some(peer_state) = self.state.peers.get_mut(&peer) {
            peer_state.finish_serving_headers();
        }
    }

    async fn handle_wire_frame(&mut self, peer: ZakuraPeerId, frame: Frame) {
        if u8::try_from(frame.message_type).ok() != Some(MSG_HS_HEADERS) {
            match HeaderSyncMessage::decode_frame(frame, HeaderSyncDecodeContext::control()) {
                Ok(msg) => self.handle_wire_message(peer, msg).await,
                Err(error) => {
                    self.trace_peer_violation(&peer, HeaderSyncMisbehavior::MalformedMessage);
                    tracing::debug!(?peer, ?error, "malformed Zakura header-sync frame");
                    self.report_misbehavior(peer, HeaderSyncMisbehavior::MalformedMessage)
                        .await;
                }
            }
            return;
        }

        // `Headers` response decode still depends on this actor's per-peer
        // outstanding-request state. The per-peer concurrency epic moves that
        // contract into the Sink task and removes this residual raw-frame hop.
        metrics::counter!("sync.header.response.received").increment(1);
        let Some(peer_state) = self.state.peers.get_mut(&peer) else {
            self.report_misbehavior(peer, HeaderSyncMisbehavior::UnsolicitedHeaders)
                .await;
            return;
        };
        let Some(outstanding) = peer_state.pop_oldest_outstanding() else {
            if peer_state.take_late_covered_response() {
                return;
            }
            self.report_misbehavior(peer, HeaderSyncMisbehavior::UnsolicitedHeaders)
                .await;
            return;
        };
        let peer_max_headers_per_response = peer_state.max_headers_per_response;
        let in_flight_count = peer_state.outstanding.len();

        let decode_context = HeaderSyncDecodeContext::for_headers_response(
            HeaderSyncRequestContract::new(
                outstanding.range.start_height,
                outstanding.expected_max_count,
            )
            .expect("outstanding range uses a non-zero bounded count"),
            peer_max_headers_per_response,
        );

        let headers = match HeaderSyncMessage::decode_frame(frame, decode_context) {
            Ok(HeaderSyncMessage::Headers(headers)) => headers,
            Ok(_) => {
                self.report_misbehavior(peer.clone(), HeaderSyncMisbehavior::MalformedMessage)
                    .await;
                self.state.schedule.retry(outstanding.range);
                self.schedule().await;
                return;
            }
            Err(error) => {
                self.trace_peer_violation(&peer, HeaderSyncMisbehavior::MalformedMessage);
                tracing::debug!(?peer, ?error, "malformed Zakura header-sync frame");
                self.report_misbehavior(peer.clone(), HeaderSyncMisbehavior::MalformedMessage)
                    .await;
                self.state.schedule.retry(outstanding.range);
                self.schedule().await;
                return;
            }
        };

        self.handle_headers_for_outstanding(
            peer,
            headers,
            outstanding,
            peer_max_headers_per_response,
            in_flight_count,
        )
        .await;
    }

    async fn handle_wire_message(&mut self, peer: ZakuraPeerId, msg: HeaderSyncMessage) {
        match msg {
            HeaderSyncMessage::Status(status) => {
                metrics::counter!("sync.header.peer.status.received").increment(1);
                if status.anchor_height > status.tip_height {
                    self.report_misbehavior(peer, HeaderSyncMisbehavior::InvalidStatus)
                        .await;
                    return;
                }

                let Some(peer_state) = self.state.peers.get_mut(&peer) else {
                    return;
                };
                if !peer_state.inbound_status.try_take(Instant::now()) {
                    self.report_misbehavior(peer, HeaderSyncMisbehavior::StatusSpam)
                        .await;
                    return;
                }
                peer_state.advertised_tip = status.tip_height;
                peer_state.anchor = status.anchor_height;
                peer_state.max_headers_per_response =
                    clamp_advertised_range(status.max_headers_per_response);
                peer_state.max_inflight_requests = status
                    .max_inflight_requests
                    .clamp(1, LOCAL_MAX_HS_INFLIGHT_PER_PEER);
                peer_state.received_status = true;
                self.trace_status_received(&peer, status);
                self.schedule().await;
            }
            HeaderSyncMessage::Headers(headers) => {
                self.handle_headers(peer, headers).await;
            }
            HeaderSyncMessage::GetHeaders {
                start_height,
                count,
            } => {
                self.handle_get_headers(peer, start_height, count).await;
            }
            HeaderSyncMessage::NewBlock(block) => {
                self.handle_new_block(peer, block).await;
            }
        }
    }

    async fn handle_get_headers(
        &mut self,
        peer: ZakuraPeerId,
        start_height: block::Height,
        count: u32,
    ) {
        let local_inflight_cap = self.startup.config.advertised_max_inflight_requests();
        let Some(peer_state) = self.state.peers.get_mut(&peer) else {
            self.report_misbehavior(peer, HeaderSyncMisbehavior::GetHeadersSpam)
                .await;
            return;
        };

        if !peer_state.received_status {
            self.report_misbehavior(peer, HeaderSyncMisbehavior::GetHeadersSpam)
                .await;
            return;
        }

        let allowed_count = inbound_get_headers_count_limit(
            &self.startup.config,
            &self.startup.network,
            self.startup.max_frame_bytes,
        );
        if count == 0 || count > allowed_count {
            self.report_misbehavior(peer, HeaderSyncMisbehavior::GetHeadersTooLong)
                .await;
            return;
        }

        if !peer_state.try_start_serving_headers(local_inflight_cap) {
            self.report_misbehavior(peer, HeaderSyncMisbehavior::GetHeadersSpam)
                .await;
            return;
        }

        if self
            .actions
            .send(HeaderSyncAction::QueryHeadersByHeightRange {
                peer: peer.clone(),
                start: start_height,
                count,
            })
            .await
            .is_err()
        {
            if let Some(peer_state) = self.state.peers.get_mut(&peer) {
                peer_state.finish_serving_headers();
            }
        }
    }

    #[tracing::instrument(skip(self, block))]
    async fn handle_new_block(&mut self, peer: ZakuraPeerId, block: Arc<block::Block>) {
        metrics::counter!("sync.header.tip.new_block.received").increment(1);

        if !self.state.peers.contains_key(&peer) {
            self.report_misbehavior(peer, HeaderSyncMisbehavior::UnknownPeer)
                .await;
            return;
        }

        let hash = block.hash();
        let Some(height) = block.coinbase_height() else {
            self.report_misbehavior(peer, HeaderSyncMisbehavior::MalformedMessage)
                .await;
            return;
        };
        self.trace_new_block_received(&peer, height, hash);

        if self.state.seen.contains(&hash) {
            metrics::counter!("sync.header.tip.new_block.deduped").increment(1);
            self.trace_new_block_deduped(&peer, height, hash, "seen_cache");
            return;
        }
        if self.state.pending_new_blocks.contains(&hash) {
            metrics::counter!("sync.header.tip.new_block.deduped").increment(1);
            self.trace_new_block_deduped(&peer, height, hash, "pending_acceptance");
            return;
        }

        if !self
            .state
            .peers
            .get_mut(&peer)
            .expect("peer exists because it was checked before validation")
            .inbound_new_block
            .try_take(Instant::now())
        {
            self.report_misbehavior(peer, HeaderSyncMisbehavior::NewBlockSpam)
                .await;
            return;
        }

        if validate_new_block_stateless(block.clone(), &self.startup.network, Utc::now(), height)
            .await
            .is_err()
        {
            self.report_misbehavior(peer, HeaderSyncMisbehavior::InvalidNewBlock)
                .await;
            return;
        }

        if !self.startup.inbound_new_block_acceptance_enabled {
            metrics::counter!("sync.header.tip.new_block.acceptance_unavailable").increment(1);
            debug!(
                ?peer,
                ?hash,
                "Zakura header-sync NewBlock body suppressed until block acceptance is wired"
            );
            return;
        }

        let inserted = self.state.pending_new_blocks.insert(hash);
        debug_assert!(inserted, "pending acceptance was checked before insert");

        if self
            .actions
            .send(HeaderSyncAction::NewBlockReceived {
                peer,
                height,
                hash,
                block,
            })
            .await
            .is_err()
        {
            self.state.pending_new_blocks.remove(&hash);
        }
    }

    fn eligible_tip_destinations(
        &self,
        source: &ZakuraPeerId,
        height: block::Height,
    ) -> Vec<ZakuraPeerId> {
        let mut peers: Vec<_> = self
            .state
            .peers
            .iter()
            .filter(|(peer_id, peer)| {
                *peer_id != source && (!peer.received_status || peer.advertised_tip < height)
            })
            .map(|(peer_id, _)| peer_id.clone())
            .collect();
        peers.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        peers
    }

    #[tracing::instrument(skip(self, headers))]
    async fn handle_headers(&mut self, peer: ZakuraPeerId, headers: Vec<Arc<block::Header>>) {
        metrics::counter!("sync.header.response.received").increment(1);
        let Some(peer_state) = self.state.peers.get_mut(&peer) else {
            self.report_misbehavior(peer, HeaderSyncMisbehavior::UnsolicitedHeaders)
                .await;
            return;
        };
        let Some(outstanding) = peer_state.pop_oldest_outstanding() else {
            if peer_state.take_late_covered_response() {
                return;
            }
            self.report_misbehavior(peer, HeaderSyncMisbehavior::UnsolicitedHeaders)
                .await;
            return;
        };
        let peer_max_headers_per_response = peer_state.max_headers_per_response;
        let in_flight_count = peer_state.outstanding.len();

        self.handle_headers_for_outstanding(
            peer,
            headers,
            outstanding,
            peer_max_headers_per_response,
            in_flight_count,
        )
        .await;
    }

    async fn handle_headers_for_outstanding(
        &mut self,
        peer: ZakuraPeerId,
        headers: Vec<Arc<block::Header>>,
        outstanding: OutstandingRange,
        peer_max_headers_per_response: u32,
        in_flight_count: usize,
    ) {
        if headers.is_empty() {
            let deadline = Instant::now() + self.empty_headers_retry_delay();
            self.trace_headers_received(
                &peer,
                outstanding.range.start_height,
                0,
                outstanding.expected_max_count,
                peer_max_headers_per_response,
                in_flight_count,
            );
            if let Some(peer_state) = self.state.peers.get_mut(&peer) {
                peer_state.outstanding.push(OutstandingRange {
                    deadline,
                    clear_assignment_on_timeout: true,
                    ..outstanding
                });
            }
            return;
        }

        let header_count =
            u32::try_from(headers.len()).expect("decoded Headers length is capped by u32");
        self.trace_headers_received(
            &peer,
            outstanding.range.start_height,
            header_count,
            outstanding.expected_max_count,
            peer_max_headers_per_response,
            in_flight_count,
        );
        if header_count > outstanding.expected_max_count || header_count > outstanding.range.count {
            self.report_misbehavior(peer.clone(), HeaderSyncMisbehavior::ResponseTooLong)
                .await;
            self.state.schedule.retry(outstanding.range);
            self.schedule().await;
            return;
        }

        let validation_context = HeaderSyncValidationContext {
            network: &self.startup.network,
            now: Utc::now(),
            start_height: outstanding.range.start_height,
            decode_context: HeaderSyncDecodeContext::for_headers_response(
                HeaderSyncRequestContract::new(
                    outstanding.range.start_height,
                    outstanding.expected_max_count,
                )
                .expect("outstanding range uses a non-zero bounded count"),
                outstanding.expected_max_count,
            ),
        };
        if validate_header_range_links(outstanding.range.anchor_hash, &headers).is_err() {
            self.report_misbehavior(peer.clone(), HeaderSyncMisbehavior::InvalidRange)
                .await;
            self.state.schedule.retry(outstanding.range);
            self.schedule().await;
            return;
        }
        if validate_headers_stateless(headers.clone(), validation_context)
            .await
            .is_err()
        {
            self.report_misbehavior(peer.clone(), HeaderSyncMisbehavior::InvalidRange)
                .await;
            self.state.schedule.retry(outstanding.range);
            self.schedule().await;
            return;
        }

        let end_height = height_after_count(outstanding.range.start_height, header_count)
            .and_then(previous_height)
            .expect("non-empty bounded range has an end height");
        if outstanding.range.finalized {
            let last_hash = headers
                .last()
                .map(|header| block::Hash::from(header.as_ref()))
                .expect("headers is non-empty");
            if end_height != outstanding.range.end_height()
                || self.startup.network.checkpoint_list().hash(end_height) != Some(last_hash)
            {
                self.report_misbehavior(peer.clone(), HeaderSyncMisbehavior::InvalidRange)
                    .await;
                self.state.schedule.retry(outstanding.range);
                self.schedule().await;
                return;
            }
        }

        self.state.pending_commits.insert(
            PendingCommitKey {
                peer: peer.clone(),
                start_height: outstanding.range.start_height,
                count: header_count,
            },
            outstanding.range,
        );
        let _ = self
            .actions
            .send(HeaderSyncAction::CommitHeaderRange {
                peer,
                anchor: outstanding.range.anchor_hash,
                start_height: outstanding.range.start_height,
                headers,
                finalized: outstanding.range.finalized,
            })
            .await;
    }

    async fn handle_timeouts(&mut self) {
        let now = Instant::now();
        let mut timed_out = Vec::new();
        for peer in self.state.peers.values_mut() {
            let mut index = 0;
            while index < peer.outstanding.len() {
                if peer.outstanding[index].deadline <= now {
                    let outstanding = peer.outstanding.remove(index);
                    timed_out.push((outstanding.range, outstanding.clear_assignment_on_timeout));
                } else {
                    index += 1;
                }
            }
        }
        for (range, clear_assignment) in timed_out {
            if clear_assignment {
                self.state.schedule.clear_assignment(range);
            }
            self.state.schedule.retry(range);
        }
        self.schedule().await;
    }

    fn empty_headers_retry_delay(&self) -> Duration {
        self.startup.request_timeout.min(EMPTY_HEADERS_RETRY_DELAY)
    }

    async fn schedule(&mut self) {
        if !self.startup.range_state_actions_enabled {
            return;
        }

        self.state.refresh_forward_range(&self.startup);
        self.state.refresh_backward_range(&self.startup);

        let mut peer_ids: Vec<ZakuraPeerId> = self.state.peers.keys().cloned().collect();
        peer_ids.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));

        for peer_id in peer_ids {
            let Some(peer) = self.state.peers.get(&peer_id) else {
                continue;
            };
            if !peer.received_status || peer.available_slots() == 0 {
                continue;
            }

            let Some(mut range) = self.state.schedule.next_for_peer(&peer_id, peer) else {
                continue;
            };
            let original_range = range;

            let count = clamp_header_sync_request_count(
                range.count,
                peer.max_headers_per_response,
                &self.startup.network,
                self.startup.max_frame_bytes,
            );
            if range.finalized && count < range.count {
                self.state.schedule.retry(range);
                continue;
            }
            range.count = count;
            self.state
                .schedule
                .narrow_queued_range(original_range, range);

            let deadline = Instant::now() + self.startup.request_timeout;
            let outstanding = OutstandingRange {
                range,
                deadline,
                expected_max_count: count,
                clear_assignment_on_timeout: false,
            };
            let peer_cap = peer.max_headers_per_response;
            if let Some(peer) = self.state.peers.get_mut(&peer_id) {
                peer.outstanding.push(outstanding);
            }
            self.state.schedule.mark_assigned(peer_id.clone(), range);
            let destination = peer_id.clone();
            let _ = self
                .actions
                .send(HeaderSyncAction::SendMessage {
                    peer: peer_id,
                    msg: HeaderSyncMessage::GetHeaders {
                        start_height: range.start_height,
                        count,
                    },
                })
                .await;
            metrics::counter!("sync.header.request.sent").increment(1);
            self.trace_get_headers_sent(&destination, range.start_height, count, peer_cap);
        }
    }

    async fn send_status(&self, peer: ZakuraPeerId) {
        metrics::counter!("sync.header.peer.status.sent").increment(1);
        self.trace_status_sent(&peer, self.local_status());
        let _ = self
            .actions
            .send(HeaderSyncAction::SendMessage {
                peer,
                msg: HeaderSyncMessage::Status(self.local_status()),
            })
            .await;
    }

    async fn publish_best_tip(&mut self, height: block::Height, hash: block::Hash) {
        self.state.best_header_tip = height;
        self.state.best_header_hash = hash;
        metrics::gauge!("sync.header.best_tip.height").set(height.0 as f64);
        self.trace_frontier_advanced(height, hash);
        let _ = self.tip.send((height, hash));
        self.broadcast_status_refresh().await;
    }

    async fn broadcast_status_refresh(&mut self) {
        let now = Instant::now();
        let status = self.local_status();
        let peer_ids: Vec<_> = self
            .state
            .peers
            .iter_mut()
            .filter_map(|(peer_id, peer)| peer.unsolicited.try_take(now).then(|| peer_id.clone()))
            .collect();

        for peer in peer_ids {
            let _ = self
                .actions
                .send(HeaderSyncAction::SendMessage {
                    peer,
                    msg: HeaderSyncMessage::Status(status),
                })
                .await;
        }
    }

    async fn notify_body_gaps(&self) {
        if !self.startup.range_state_actions_enabled {
            return;
        }

        if self.state.best_header_tip > self.state.verified_block_tip {
            let from =
                next_height(self.state.verified_block_tip).unwrap_or(self.state.verified_block_tip);
            metrics::gauge!("sync.header.missing_bodies")
                .set(count_between(from, self.state.best_header_tip) as f64);
            self.trace_missing_bodies(from, self.state.best_header_tip);
            let _ = self
                .actions
                .send(HeaderSyncAction::BodyGaps {
                    from,
                    to: self.state.best_header_tip,
                })
                .await;
        }
    }

    async fn report_misbehavior(&mut self, peer: ZakuraPeerId, reason: HeaderSyncMisbehavior) {
        if let Some(peer_state) = self.state.peers.get_mut(&peer) {
            peer_state.misbehavior = peer_state.misbehavior.saturating_add(1);
        }
        metrics::counter!("sync.header.peer.disconnect").increment(1);
        self.trace_peer_violation(&peer, reason);
        self.trace_peer_disconnect_requested(&peer, reason);
        let _ = self
            .actions
            .send(HeaderSyncAction::Misbehavior { peer, reason })
            .await;
    }

    fn trace_status_sent(&self, peer: &ZakuraPeerId, status: HeaderSyncStatus) {
        self.emit_trace(hs_trace::HEADER_STATUS_SENT, |row| {
            insert_peer(row, hs_trace::PEER, peer);
            insert_height(row, hs_trace::HEIGHT, status.tip_height);
            insert_hash(row, hs_trace::HASH, status.tip_hash);
            insert_height(row, hs_trace::RANGE_START, status.anchor_height);
            insert_u64(
                row,
                hs_trace::ADVERTISED_CAP,
                u64::from(status.max_headers_per_response),
            );
            insert_u64(
                row,
                hs_trace::IN_FLIGHT_COUNT,
                u64::from(status.max_inflight_requests),
            );
        });
    }

    fn trace_status_received(&self, peer: &ZakuraPeerId, status: HeaderSyncStatus) {
        self.emit_trace(hs_trace::HEADER_STATUS_RECEIVED, |row| {
            insert_peer(row, hs_trace::PEER, peer);
            insert_height(row, hs_trace::HEIGHT, status.tip_height);
            insert_hash(row, hs_trace::HASH, status.tip_hash);
            insert_height(row, hs_trace::RANGE_START, status.anchor_height);
            insert_u64(
                row,
                hs_trace::ADVERTISED_CAP,
                u64::from(status.max_headers_per_response),
            );
            insert_u64(
                row,
                hs_trace::IN_FLIGHT_COUNT,
                u64::from(status.max_inflight_requests),
            );
        });
    }

    fn trace_get_headers_sent(
        &self,
        peer: &ZakuraPeerId,
        start_height: block::Height,
        count: u32,
        advertised_cap: u32,
    ) {
        self.emit_trace(hs_trace::HEADER_GET_HEADERS_SENT, |row| {
            insert_peer(row, hs_trace::PEER, peer);
            insert_height(row, hs_trace::RANGE_START, start_height);
            insert_u64(row, hs_trace::RANGE_COUNT, u64::from(count));
            insert_u64(row, hs_trace::ADVERTISED_CAP, u64::from(advertised_cap));
        });
    }

    fn trace_headers_received(
        &self,
        peer: &ZakuraPeerId,
        start_height: block::Height,
        count: u32,
        expected_max_count: u32,
        advertised_cap: u32,
        in_flight_count: usize,
    ) {
        self.emit_trace(hs_trace::HEADER_HEADERS_RECEIVED, |row| {
            insert_peer(row, hs_trace::PEER, peer);
            insert_height(row, hs_trace::RANGE_START, start_height);
            insert_u64(row, hs_trace::RANGE_COUNT, u64::from(count));
            insert_u64(row, hs_trace::ADVERTISED_CAP, u64::from(advertised_cap));
            insert_u64(row, hs_trace::EXPECTED_COUNT, u64::from(expected_max_count));
            insert_u64(row, hs_trace::IN_FLIGHT_COUNT, in_flight_count as u64);
        });
    }

    fn trace_headers_served(
        &self,
        peer: &ZakuraPeerId,
        start_height: block::Height,
        requested_count: u32,
        returned_count: u32,
    ) {
        self.emit_trace(hs_trace::HEADER_HEADERS_SERVED, |row| {
            insert_peer(row, hs_trace::PEER, peer);
            insert_height(row, hs_trace::RANGE_START, start_height);
            insert_u64(row, hs_trace::RANGE_COUNT, u64::from(returned_count));
            insert_u64(row, hs_trace::EXPECTED_COUNT, u64::from(requested_count));
        });
    }

    fn trace_range_event(
        &self,
        event: &'static str,
        start_height: block::Height,
        count: u32,
        peer: Option<&ZakuraPeerId>,
        reason: Option<&'static str>,
    ) {
        self.emit_trace(event, |row| {
            if let Some(peer) = peer {
                insert_peer(row, hs_trace::PEER, peer);
            }
            insert_height(row, hs_trace::RANGE_START, start_height);
            insert_u64(row, hs_trace::RANGE_COUNT, u64::from(count));
            insert_optional_str(row, hs_trace::REASON, reason);
        });
    }

    fn trace_new_block_received(
        &self,
        peer: &ZakuraPeerId,
        height: block::Height,
        hash: block::Hash,
    ) {
        self.emit_trace(hs_trace::HEADER_NEW_BLOCK_RECEIVED, |row| {
            insert_peer(row, hs_trace::PEER, peer);
            insert_height(row, hs_trace::HEIGHT, height);
            insert_hash(row, hs_trace::HASH, hash);
        });
    }

    fn trace_new_block_forwarded(
        &self,
        source: &ZakuraPeerId,
        destination: &ZakuraPeerId,
        height: block::Height,
        hash: block::Hash,
        destination_count: usize,
    ) {
        self.emit_trace(hs_trace::HEADER_NEW_BLOCK_FORWARDED, |row| {
            insert_peer(row, hs_trace::SOURCE_PEER, source);
            insert_peer(row, hs_trace::PEER, destination);
            insert_height(row, hs_trace::HEIGHT, height);
            insert_hash(row, hs_trace::HASH, hash);
            insert_u64(
                row,
                hs_trace::DESTINATION_PEER_COUNT,
                destination_count as u64,
            );
        });
    }

    fn trace_new_block_deduped(
        &self,
        peer: &ZakuraPeerId,
        height: block::Height,
        hash: block::Hash,
        reason: &'static str,
    ) {
        self.emit_trace(hs_trace::HEADER_NEW_BLOCK_DEDUPED, |row| {
            insert_peer(row, hs_trace::PEER, peer);
            insert_height(row, hs_trace::HEIGHT, height);
            insert_hash(row, hs_trace::HASH, hash);
            insert_optional_str(row, hs_trace::REASON, Some(reason));
        });
    }

    fn trace_peer_violation(&self, peer: &ZakuraPeerId, reason: HeaderSyncMisbehavior) {
        self.emit_trace(hs_trace::HEADER_PEER_VIOLATION, |row| {
            insert_peer(row, hs_trace::PEER, peer);
            insert_optional_str(
                row,
                hs_trace::REASON,
                Some(misbehavior_reason_label(reason)),
            );
        });
    }

    fn trace_peer_disconnect_requested(&self, peer: &ZakuraPeerId, reason: HeaderSyncMisbehavior) {
        self.emit_trace(hs_trace::HEADER_PEER_DISCONNECT_REQUESTED, |row| {
            insert_peer(row, hs_trace::PEER, peer);
            insert_optional_str(
                row,
                hs_trace::REASON,
                Some(misbehavior_reason_label(reason)),
            );
        });
    }

    fn trace_frontier_advanced(&self, height: block::Height, hash: block::Hash) {
        self.emit_trace(hs_trace::HEADER_FRONTIER_ADVANCED, |row| {
            insert_height(row, hs_trace::HEIGHT, height);
            insert_hash(row, hs_trace::HASH, hash);
        });
    }

    fn trace_missing_bodies(&self, from: block::Height, to: block::Height) {
        self.emit_trace(hs_trace::HEADER_MISSING_BODIES_REPORTED, |row| {
            insert_height(row, hs_trace::RANGE_START, from);
            insert_u64(
                row,
                hs_trace::RANGE_COUNT,
                u64::from(count_between(from, to)),
            );
        });
    }

    fn emit_trace(
        &self,
        event: &'static str,
        build: impl FnOnce(&mut serde_json::Map<String, Value>),
    ) {
        self.startup.trace.emit_with(HEADER_SYNC_TABLE, |row| {
            row.insert(
                hs_trace::EVENT.to_string(),
                Value::String(event.to_string()),
            );
            build(row);
        });
    }

    fn local_status(&self) -> HeaderSyncStatus {
        HeaderSyncStatus {
            tip_height: self.state.best_header_tip,
            tip_hash: self.state.best_header_hash,
            anchor_height: self.state.anchor.0,
            max_headers_per_response: self.startup.config.advertised_max_headers_per_response(),
            max_inflight_requests: self.startup.config.advertised_max_inflight_requests(),
        }
    }

    fn cancel_covered_outstanding(&mut self) {
        for peer in self.state.peers.values_mut() {
            let mut index = 0;
            while index < peer.outstanding.len() {
                if self
                    .state
                    .schedule
                    .is_covered(peer.outstanding[index].range)
                {
                    peer.outstanding.remove(index);
                    peer.late_covered_responses = peer.late_covered_responses.saturating_add(1);
                } else {
                    index += 1;
                }
            }
        }
    }
}

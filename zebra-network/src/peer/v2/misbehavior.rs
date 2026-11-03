//! Misbehavior scores of version 2 peers (draft, "Misbehavior and Banning").
//!
//! Owned by one actor task and reached over a channel. Scores are keyed by
//! peer group and kept across connections, so a peer cannot shed its score by
//! reconnecting, or by being disconnected and redialled. A group whose score
//! reaches [`MAX_PEER_MISBEHAVIOR_SCORE`] is reported to the address book,
//! which bans it, and keeps its full score until the score lapses: the ban
//! is in force only from the address book's next flush of misbehavior
//! reports, and until then a connection from the group is closed on its
//! first penalty.

use std::{collections::HashMap, net::IpAddr, time::Duration};

use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};

use crate::{
    constants::{BAN_DURATION, MAX_BANNED_IPS, MAX_PEER_MISBEHAVIOR_SCORE},
    protocol::external::connection_limit_key,
    MisbehaviorReport, PeerSocketAddr,
};

#[cfg(test)]
mod tests;

/// How long a connection waits for the score task to answer. The task answers
/// at once unless it has exited, so this only bounds a shutdown race.
const PENALIZE_TIMEOUT: Duration = Duration::from_secs(5);

/// A request to the score task: add `points` to `addr`'s group, and reply
/// whether the group reached the ban threshold.
struct Penalize {
    addr: PeerSocketAddr,
    points: u32,
    reason: String,
    reply: oneshot::Sender<bool>,
}

/// A handle to the misbehavior score task.
///
/// Cloning the handle shares the scores. The task runs until every handle has
/// been dropped.
#[derive(Clone, Debug)]
pub struct MisbehaviorScores {
    commands: mpsc::Sender<Penalize>,
}

impl MisbehaviorScores {
    /// Spawns a score task that reports peer groups reaching the ban
    /// threshold to `reports`, the address book's misbehavior channel.
    ///
    /// `connection_limit` sizes the queue of penalties waiting for the task;
    /// while it is full, callers wait, up to a timeout.
    pub fn spawn(reports: mpsc::Sender<MisbehaviorReport>, connection_limit: usize) -> Self {
        let (commands, receiver) = mpsc::channel(connection_limit.max(1));
        tokio::spawn(run(receiver, reports, MAX_BANNED_IPS));
        MisbehaviorScores { commands }
    }

    /// Adds `points` to `addr`'s peer group because of `reason`.
    ///
    /// Returns `true` if the group's score is at the ban threshold, whether
    /// this penalty reached it or an earlier one did, so the connection must
    /// be closed with `MISBEHAVIOR`. If the score task cannot be reached in
    /// time, the penalty is lost and this returns `false`.
    pub async fn penalize(
        &self,
        addr: PeerSocketAddr,
        points: u32,
        reason: impl Into<String>,
    ) -> bool {
        let (reply, answer) = oneshot::channel();
        let command = Penalize {
            addr,
            points,
            reason: reason.into(),
            reply,
        };
        let penalize = async {
            self.commands.send(command).await.ok()?;
            answer.await.ok()
        };
        tokio::time::timeout(PENALIZE_TIMEOUT, penalize)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| {
                tracing::debug!(?addr, "misbehavior score task is unavailable");
                false
            })
    }
}

/// A peer group's score, when the group's first penalty in the current
/// window was assigned, and whether the group has been reported for banning.
#[derive(Clone, Copy, Debug)]
struct Score {
    points: u32,
    since: Instant,
    /// Whether the group was reported to the address book in this window. A
    /// reported group keeps its score until the window ends, so each of its
    /// penalties closes the connection again, without another report.
    reported: bool,
}

impl Score {
    /// Whether the window that started at `since` has ended by `now`: scores
    /// last about as long as a ban (draft, "Misbehavior and Banning").
    fn has_lapsed(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.since) >= BAN_DURATION
    }
}

/// The score task: owns the scores and answers penalties in order.
///
/// At most `limit` peer groups are scored at once.
async fn run(
    mut commands: mpsc::Receiver<Penalize>,
    reports: mpsc::Sender<MisbehaviorReport>,
    limit: usize,
) {
    let mut scores: HashMap<IpAddr, Score> = HashMap::new();

    while let Some(Penalize {
        addr,
        points,
        reason,
        reply,
    }) = commands.recv().await
    {
        let group = connection_limit_key(addr.ip());
        let score = add_points(&mut scores, group, points, Instant::now(), limit);
        let reached_threshold = score.points >= MAX_PEER_MISBEHAVIOR_SCORE;
        if reached_threshold && !score.reported {
            // The address book only acts on a full score, and bans the whole group.
            match reports.try_send(MisbehaviorReport::new(
                addr,
                MAX_PEER_MISBEHAVIOR_SCORE,
                reason,
            )) {
                // # Security
                //
                // The score stays at the threshold until it lapses. The ban is in force only
                // from the address book's next flush of misbehavior reports, so a peer that
                // reconnects before then would otherwise start from zero, with a threshold's
                // worth of misbehavior before its next report; instead its first penalty closes
                // the connection again.
                Ok(()) => score.reported = true,
                // The score stays at the threshold, so the group's next penalty reports it again.
                Err(error) => {
                    tracing::warn!(?addr, %error, "could not report a misbehaving v2 peer");
                }
            }
        }
        // A dropped reply means the request task was cancelled; the score stands.
        let _ = reply.send(reached_threshold);
    }
}

/// Adds `points` to `group`'s score at `now`, and returns the score.
///
/// A lapsed score starts again from zero, unreported. If `limit` groups are
/// already scored, lapsed scores are dropped, then the score whose window
/// started first, so the map stays bounded however many groups misbehave.
fn add_points(
    scores: &mut HashMap<IpAddr, Score>,
    group: IpAddr,
    points: u32,
    now: Instant,
    limit: usize,
) -> &mut Score {
    if !scores.contains_key(&group) && scores.len() >= limit {
        scores.retain(|_group, score| !score.has_lapsed(now));
        if scores.len() >= limit {
            if let Some(oldest) = scores
                .iter()
                .min_by_key(|(_group, score)| score.since)
                .map(|(group, _score)| *group)
            {
                scores.remove(&oldest);
            }
        }
    }

    let fresh = Score {
        points: 0,
        since: now,
        reported: false,
    };
    let score = scores.entry(group).or_insert(fresh);
    if score.has_lapsed(now) {
        *score = fresh;
    }
    score.points = score.points.saturating_add(points);
    score
}

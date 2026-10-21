//! Nonces this node sent in `init` records, for the draft's self-connection
//! rule ("Init Record"). Owned by one actor task and reached over a channel.

use indexmap::IndexSet;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

use crate::protocol::external::types::Nonce;

#[cfg(test)]
mod tests;

/// A request to the registry task.
enum Command {
    /// Adds a nonce this node is about to send.
    Register(Nonce),

    /// Reports whether a nonce received from a peer is one this node sent.
    Contains(Nonce, oneshot::Sender<bool>),
}

/// A handle to the nonce registry task.
///
/// Cloning the handle shares the registry. The task runs until every handle
/// has been dropped.
#[derive(Clone, Debug)]
pub struct NonceRegistry {
    commands: mpsc::Sender<Command>,
}

/// The registry task was not reachable.
#[derive(Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error("the nonce registry task has exited")]
pub struct NonceRegistryError;

impl NonceRegistry {
    /// Spawns a registry that keeps the most recent `limit` nonces; callers
    /// pass their connection limit.
    ///
    /// Nonces are never removed, as in the legacy handshake: the set is
    /// bounded by evicting the oldest, which keeps memory small and the
    /// collision probability negligible.
    pub fn spawn(limit: usize) -> Self {
        let limit = limit.max(1);
        // In-flight handshakes hold connection slots, so there are at most `limit` of them, and
        // each has at most two commands outstanding: the channel does not fill. If it ever did,
        // `register` and `contains` would wait, bounded by the handshake deadline.
        let (commands, receiver) = mpsc::channel(limit * 4 + 16);
        tokio::spawn(run(receiver, limit));
        NonceRegistry { commands }
    }

    /// Records `nonce` as sent by this node.
    ///
    /// Commands are handled in order, so a later [`contains`](Self::contains)
    /// sees it; the wait is bounded by the caller's handshake deadline.
    pub async fn register(&self, nonce: Nonce) -> Result<(), NonceRegistryError> {
        self.commands
            .send(Command::Register(nonce))
            .await
            .map_err(|_| NonceRegistryError)
    }

    /// Whether `nonce` is one this node recently sent.
    ///
    /// The wait is bounded by the caller's handshake deadline.
    pub async fn contains(&self, nonce: Nonce) -> Result<bool, NonceRegistryError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Command::Contains(nonce, reply))
            .await
            .map_err(|_| NonceRegistryError)?;
        answer.await.map_err(|_| NonceRegistryError)
    }
}

/// The registry task: owns the set and answers commands in order.
async fn run(mut commands: mpsc::Receiver<Command>, limit: usize) {
    let mut nonces: IndexSet<Nonce> = IndexSet::new();

    while let Some(command) = commands.recv().await {
        match command {
            Command::Register(nonce) => {
                // A repeat of a nonce already in the set (a 2^-64 event per handshake) keeps its
                // old position, so it is evicted as the older one; the legacy handshake fails
                // such a handshake instead.
                nonces.insert(nonce);
                while nonces.len() > limit {
                    nonces.shift_remove_index(0);
                }
            }
            Command::Contains(nonce, reply) => {
                // A dropped reply means the handshake gave up; nothing to do.
                let _ = reply.send(nonces.contains(&nonce));
            }
        }
    }
}

//! Orchard and Ironwood note commitment tree updates for transactions that can be in the next
//! block, computed before that block arrives.

use std::sync::{mpsc, Arc, OnceLock};

use tokio::sync::watch;

use zebra_chain::{
    orchard,
    parallel::tree::PrecomputedTreeUpdate,
    transaction::{self, Transaction},
};

use crate::{
    constants::{PRECOMPUTED_TREE_UPDATE_NOTE_COMMITMENTS, PRECOMPUTED_TREE_UPDATE_TRANSACTIONS},
    service::non_finalized_state::NonFinalizedState,
    WatchReceiver,
};

#[cfg(test)]
mod tests;

/// The precomputed updates of the tip's Orchard and Ironwood note commitment trees.
///
/// A block appends the note commitments of its transactions to each tree in the order it holds
/// them, so an update is only useful for one order of one set of transactions. The updates are
/// computed for the transactions that the mempool last said it expects in the next block, in
/// every order of every one, two and three of them.
///
/// The mempool names its likeliest transactions each time they can have changed. The updates
/// of a transaction it no longer names are dropped. It says nothing while it has no such
/// transactions, so the last updates stay until then, where no block can use them once the
/// tip's tree changes.
#[derive(Clone, Debug, Default)]
pub(crate) struct PrecomputedTrees {
    /// The updates of the Orchard tree.
    orchard: TreeUpdates,

    /// The updates of the Ironwood tree.
    ironwood: TreeUpdates,
}

impl PrecomputedTrees {
    /// Returns every precomputed update.
    pub(crate) fn updates(&self) -> Vec<Arc<PrecomputedTreeUpdate>> {
        self.orchard
            .updates
            .iter()
            .chain(&self.ironwood.updates)
            .map(|(_transactions, update)| update.clone())
            .collect()
    }
}

/// The precomputed updates of one tree.
#[derive(Clone, Debug, Default)]
struct TreeUpdates {
    /// The tree the updates append to, which was the tip's tree when they were computed.
    parent: Option<Arc<orchard::tree::NoteCommitmentTree>>,

    /// The transactions the updates are for, with the note commitments they append.
    transactions: Vec<(transaction::Hash, Vec<orchard::tree::NoteCommitmentUpdate>)>,

    /// The updates, each with the transactions whose note commitments it appends, in order.
    updates: Vec<(Vec<transaction::Hash>, Arc<PrecomputedTreeUpdate>)>,
}

impl TreeUpdates {
    /// Makes these the updates of `parent` that a block can make with the transactions of
    /// `expected`, which has each transaction's ID and the note commitments it appends to this
    /// tree, the likeliest transaction last.
    ///
    /// The updates are for the last [`PRECOMPUTED_TREE_UPDATE_TRANSACTIONS`] of the transactions
    /// that append note commitments, and at most
    /// [`PRECOMPUTED_TREE_UPDATE_NOTE_COMMITMENTS`] of them. The updates of those transactions
    /// that are already here are kept, the missing ones are computed, and all others are
    /// dropped, as are the updates of another tree.
    fn expect(
        &mut self,
        parent: &Arc<orchard::tree::NoteCommitmentTree>,
        expected: impl IntoIterator<
            Item = (transaction::Hash, Vec<orchard::tree::NoteCommitmentUpdate>),
        >,
    ) {
        let has_parent = self
            .parent
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, parent) || current == parent);
        if !has_parent {
            *self = Self {
                parent: Some(parent.clone()),
                ..Self::default()
            };
        }

        let mut wanted: Vec<_> = Vec::new();
        for (id, note_commitments) in expected {
            if note_commitments.is_empty()
                || note_commitments.len() > PRECOMPUTED_TREE_UPDATE_NOTE_COMMITMENTS
            {
                continue;
            }
            // A transaction that is named twice keeps its later, likelier place.
            wanted.retain(|(known, _)| *known != id);
            wanted.push((id, note_commitments));
            // Only the last transactions are wanted, however many there are.
            if wanted.len() > PRECOMPUTED_TREE_UPDATE_TRANSACTIONS {
                wanted.remove(0);
            }
        }

        let is_wanted = |id: &transaction::Hash| wanted.iter().any(|(wanted, _)| wanted == id);
        self.transactions.retain(|(id, _)| is_wanted(id));
        self.updates
            .retain(|(order, _update)| order.iter().all(is_wanted));

        for (id, note_commitments) in wanted {
            if !self.transactions.iter().any(|(known, _)| *known == id) {
                self.add_transaction(parent, id, note_commitments);
            }
        }
    }

    /// Computes the updates of `parent` that a block with the transaction `id` can make.
    ///
    /// These are the update that appends `note_commitments` alone, and an update for each place
    /// the transaction can take among the transactions of each update that is already here.
    fn add_transaction(
        &mut self,
        parent: &Arc<orchard::tree::NoteCommitmentTree>,
        id: transaction::Hash,
        note_commitments: Vec<orchard::tree::NoteCommitmentUpdate>,
    ) {
        self.transactions.push((id, note_commitments));

        let orders: Vec<Vec<transaction::Hash>> = std::iter::once(Vec::new())
            .chain(self.updates.iter().map(|(order, _update)| order.clone()))
            .collect();

        for order in orders {
            for position in 0..=order.len() {
                let mut order = order.clone();
                order.insert(position, id);
                let note_commitments = order
                    .iter()
                    .flat_map(|transaction| {
                        self.transactions
                            .iter()
                            .filter(move |(known, _)| known == transaction)
                            .flat_map(|(_, note_commitments)| note_commitments.iter().copied())
                    })
                    .collect();

                // A tree without room for the note commitments has no update.
                if let Ok(update) = PrecomputedTreeUpdate::new(parent.clone(), note_commitments) {
                    self.updates.push((order, Arc::new(update)));
                }
            }
        }
    }
}

/// The transactions that the mempool expects in the next block.
#[derive(Clone, Debug)]
struct ExpectedTransactions {
    /// The state whose tip the updates are for, and which keeps them.
    non_finalized_state: WatchReceiver<NonFinalizedState>,

    /// The transactions and their IDs, the likeliest last.
    transactions: Vec<(transaction::Hash, Arc<Transaction>)>,
}

/// Precomputes tree updates for transactions that can be in the next block, on a thread of its
/// own.
///
/// The thread starts with the first request, and ends when the last clone of this handle is
/// dropped. A handle and its clones serve one state: its services share the thread through
/// their clones of the handle.
#[derive(Clone, Debug, Default)]
pub(crate) struct TreePrecomputer {
    /// The thread's wake-up channel, and the last request, which the thread takes.
    ///
    /// Each request replaces the one before it, so the thread only ever needs the last one.
    thread: Arc<
        OnceLock<(
            mpsc::SyncSender<()>,
            watch::Sender<Option<ExpectedTransactions>>,
        )>,
    >,
}

impl TreePrecomputer {
    /// Asks for `transactions`, the likeliest last, to have the updates they can make to the
    /// Orchard and Ironwood trees of the best chain tip of `non_finalized_state` computed, and
    /// kept in that state in place of the updates it has.
    ///
    /// Does nothing if none of the transactions has Orchard or Ironwood note commitments.
    pub(crate) fn transactions_expected(
        &self,
        non_finalized_state: WatchReceiver<NonFinalizedState>,
        transactions: impl IntoIterator<Item = (transaction::Hash, Arc<Transaction>)>,
    ) {
        let transactions: Vec<_> = transactions
            .into_iter()
            .filter(|(_id, transaction)| {
                transaction.has_orchard_shielded_data() || transaction.has_ironwood_shielded_data()
            })
            .collect();
        if transactions.is_empty() {
            return;
        }

        let (wake, last_request) = self.thread.get_or_init(|| {
            // One waiting wake-up is enough: the thread takes the last request when it wakes.
            let (wake, woken) = mpsc::sync_channel(1);
            let last_request = watch::Sender::new(None);
            let requests = last_request.subscribe();
            let thread = std::thread::Builder::new()
                .name("tree precompute".to_string())
                .spawn(move || precompute_tree_updates(woken, requests));
            if let Err(error) = thread {
                // No updates are precomputed, and blocks compute theirs.
                tracing::warn!(
                    ?error,
                    "could not start the thread that precomputes note commitment tree updates"
                );
            }
            (wake, last_request)
        });

        last_request.send_replace(Some(ExpectedTransactions {
            non_finalized_state,
            transactions,
        }));
        // The thread is already going to wake up if this fails, or it couldn't be started.
        let _ = wake.try_send(());
    }

    /// Returns true if the thread that computes the updates was ever asked for any.
    #[cfg(test)]
    pub(crate) fn has_requests(&self) -> bool {
        self.thread.get().is_some()
    }
}

/// Keeps the tree updates of the transactions that were last expected in a state, each time it
/// is woken.
///
/// The updates are for the trees of that state's best chain tip when the thread takes the
/// request, which can be a later tip than the one the mempool had when it sent the request.
fn precompute_tree_updates(
    woken: mpsc::Receiver<()>,
    mut requests: watch::Receiver<Option<ExpectedTransactions>>,
) {
    while woken.recv().is_ok() {
        let Some(expected) = requests.borrow_and_update().clone() else {
            continue;
        };

        let (precomputed_sender, tip_trees) =
            expected
                .non_finalized_state
                .borrow_mapped(|non_finalized_state| {
                    (
                        non_finalized_state.precomputed_trees().clone(),
                        non_finalized_state.best_chain().map(|chain| {
                            (
                                chain.orchard_note_commitment_tree_for_tip(),
                                chain.ironwood_note_commitment_tree_for_tip(),
                            )
                        }),
                    )
                });
        // Nothing is precomputed while the best chain is only on disk, which avoids reading its
        // trees.
        let Some((orchard, ironwood)) = tip_trees else {
            continue;
        };

        let mut precomputed = precomputed_sender.borrow().clone();

        precomputed.orchard.expect(
            &orchard,
            expected.transactions.iter().map(|(id, transaction)| {
                (
                    *id,
                    PrecomputedTreeUpdate::orchard_note_commitments(transaction),
                )
            }),
        );
        precomputed.ironwood.expect(
            &ironwood,
            expected.transactions.iter().map(|(id, transaction)| {
                (
                    *id,
                    PrecomputedTreeUpdate::ironwood_note_commitments(transaction),
                )
            }),
        );

        precomputed_sender.send_replace(precomputed);
    }
}

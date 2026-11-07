//! The synchronization artifacts this node serves through `get-object`
//! requests (draft, "`get-object`"), and how the serving code learns about
//! them.
//!
//! Objects are content-addressed: each is a file whose name is the hex of its
//! SHA-256 hash, so serving is "open by hash, read a range". The index of the
//! files is published through a watch channel, so the component that verifies
//! and stores the files can replace the index without locks, and every
//! connection reads the latest one.

use std::{collections::HashMap, ops::Deref, path::PathBuf, sync::Arc};

use tokio::sync::watch;

pub use zebra_chain::state_snapshot::ObjectHash;

/// Where a served object is, and how long it is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectEntry {
    /// The file holding the object's complete contents.
    pub path: PathBuf,

    /// The object's size in bytes: the `size` of every `get-object` answer for it.
    pub size: u64,
}

/// The served objects, by hash.
pub type ObjectIndex = HashMap<ObjectHash, ObjectEntry>;

/// The receiving end of the published [`ObjectIndex`]: a connection reads the
/// current index for each `get-object` request, and the publisher replaces it
/// whenever its verified objects change.
///
/// Two sources are equal when they watch the same channel, which lets the
/// network [`Config`](crate::Config) that carries one keep deriving equality.
#[derive(Clone, Debug)]
pub struct ObjectSource(pub watch::Receiver<Arc<ObjectIndex>>);

impl ObjectSource {
    /// Returns a source that watches `index`, and the sender that replaces it.
    pub fn channel(index: Arc<ObjectIndex>) -> (watch::Sender<Arc<ObjectIndex>>, Self) {
        let (sender, receiver) = watch::channel(index);
        (sender, ObjectSource(receiver))
    }
}

impl Deref for ObjectSource {
    type Target = watch::Receiver<Arc<ObjectIndex>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl PartialEq for ObjectSource {
    fn eq(&self, other: &Self) -> bool {
        self.0.same_channel(&other.0)
    }
}

impl Eq for ObjectSource {}

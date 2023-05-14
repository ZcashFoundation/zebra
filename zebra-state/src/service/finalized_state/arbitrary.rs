//! Arbitrary value generation and test harnesses for the finalized state.

#![allow(dead_code)]

use std::{ops::Deref, sync::Arc};

use crate::service::finalized_state::{
    disk_format::{FromDisk, TryIntoDisk},
    FinalizedState, ZebraDb,
};

// Enable older test code to automatically access the inner database via Deref coercion.
impl Deref for FinalizedState {
    type Target = ZebraDb;

    fn deref(&self) -> &Self::Target {
        &self.db
    }
}

pub fn round_trip<T>(input: T) -> Option<T>
where
    T: TryIntoDisk + FromDisk,
{
    let bytes = input.try_as_bytes()?;
    Some(T::from_bytes(bytes))
}

pub fn assert_round_trip<T>(input: T)
where
    T: TryIntoDisk + FromDisk + Clone + PartialEq + std::fmt::Debug,
{
    let before = Some(input.clone());
    let after = round_trip(input);
    assert_eq!(before, after);
}

pub fn round_trip_ref<T>(input: &T) -> Option<T>
where
    T: TryIntoDisk + FromDisk,
{
    let bytes = input.try_as_bytes()?;
    Some(T::from_bytes(bytes))
}

pub fn assert_round_trip_ref<T>(input: &T)
where
    T: TryIntoDisk + FromDisk + Clone + PartialEq + std::fmt::Debug,
{
    let before = Some(input.clone());
    let after = round_trip_ref(input);
    assert_eq!(before, after);
}

pub fn round_trip_arc<T>(input: Arc<T>) -> Option<Arc<T>>
where
    T: TryIntoDisk + FromDisk,
{
    let bytes = input.try_as_bytes()?;
    Some(Arc::new(T::from_bytes(bytes)))
}

pub fn assert_round_trip_arc<T>(input: Arc<T>)
where
    T: TryIntoDisk + FromDisk + Clone + PartialEq + std::fmt::Debug,
{
    let before = Some(input.clone());
    let after = round_trip_arc(input);
    assert_eq!(before, after);
}

/// The round trip test covers types that are used as value field in a rocksdb
/// column family. Only these types are ever deserialized, and so they're the only
/// ones that implement both `IntoDisk` and `FromDisk`.
pub fn assert_value_properties<T>(input: T)
where
    T: TryIntoDisk + FromDisk + Clone + PartialEq + std::fmt::Debug,
{
    assert_round_trip_ref(&input);
    assert_round_trip_arc(Arc::new(input.clone()));
    assert_round_trip(input);
}

use std::{
    fmt,
    io::{self},
};

#[cfg(test)]
use proptest::{array, collection::vec, prelude::*};
#[cfg(test)]
use proptest_derive::Arbitrary;

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

#[cfg(test)]
proptest! {

    // #[test]
    // fn test() {}
}

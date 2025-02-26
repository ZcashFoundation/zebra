/// FIXME: refactor orchard_zsa/tests (possibly move vectors to zebra-tests), remove cfg(test) here etc.
#[cfg(test)]
mod blocks;

/// FIXME: pub is needed to access test vectors from other crates, remove it then
pub mod vectors;

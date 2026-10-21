//! Tests for the version 2 protocol.

use std::sync::Arc;

use bytes::BytesMut;
use tokio_util::codec::Decoder;

use zebra_chain::{block::Block, serialization::ZcashDeserialize};

/// Asserts that `$result` is an error matching `$pattern` (optionally with a
/// guard) and, when a third argument is given, carrying that `violation()`.
macro_rules! assert_err {
    ($result:expr, $pattern:pat $(if $guard:expr)? $(, $violation:expr)? $(,)?) => {{
        let error = $result
            .expect_err(concat!("expected an error matching ", stringify!($pattern)));
        assert!(
            matches!(error, $pattern $(if $guard)?),
            "expected {}, got {error:?}",
            stringify!($pattern)
        );
        $( assert_eq!(error.violation(), Some($violation), "got {error:?}"); )?
    }};
}

mod prop;
mod vectors;

/// Parses a test vector block.
fn test_block(bytes: &[u8]) -> Arc<Block> {
    Arc::new(Block::zcash_deserialize(bytes).expect("test vectors are valid blocks"))
}

/// Feeds `wire` to `decoder` in the given chunk sizes, cycling through them,
/// collecting items, and finishes with `decode_eof`.
fn decode_in_chunks<D: Decoder>(
    decoder: &mut D,
    wire: &[u8],
    chunk_lens: &[usize],
) -> Result<Vec<D::Item>, D::Error> {
    let mut src = BytesMut::new();
    let mut items = Vec::new();
    let mut offset = 0;
    for chunk_len in chunk_lens.iter().map(|len| (*len).max(1)).cycle() {
        if offset >= wire.len() {
            break;
        }
        let end = (offset + chunk_len).min(wire.len());
        src.extend_from_slice(&wire[offset..end]);
        offset = end;
        while let Some(item) = decoder.decode(&mut src)? {
            items.push(item);
        }
    }
    while let Some(item) = decoder.decode_eof(&mut src)? {
        items.push(item);
    }
    Ok(items)
}

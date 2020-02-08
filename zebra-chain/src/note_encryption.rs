//! Note encryption types.

use std::fmt;

/// A 512-byte _Memo_ field associated with a note, as described in
/// [protocol specification §5.5][ps].
///
/// The usage of the memo field is by agreement between the sender and
/// recipient of the note.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#spendencoding
#[derive(Clone, Copy)]
pub struct Memo([u8; 512]);

impl Memo {}

impl fmt::Debug for Memo {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let output: String;

        match String::from_utf8(self.0.to_vec()) {
            Ok(memo) => output = memo,
            _ => output = hex::encode(&self.0[..]),
        }

        f.debug_tuple("Memo").field(&output).finish()
    }
}

#[test]
fn memo_fmt() {
    let memo = Memo([0u8; 512]);

    println!("{:?}", memo);

    let memo2 = Memo(
        *b"thiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiis \
                 iiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiis \
                 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
                 veeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeryyyyyyyyyyyyyyyyyyyyyyyyyy \
                 looooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooong \
                 meeeeeeeeeeeeeeeeeeemooooooooooooooooooooooooooooooooooooooooooooooooooooooooooo \
                 but it's just short enough",
    );

    println!("{:?}", memo2);
}

//! Note encryption types.

use std::{
    convert::{AsRef, From},
    fmt,
};

mod sapling;
mod sprout;

/// A 512-byte _Memo_ field associated with a note, as described in
/// [protocol specification §5.5][ps].
///
/// The usage of the memo field is by agreement between the sender and
/// recipient of the note.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#spendencoding
#[derive(Clone, Copy)]
pub struct Memo([u8; 512]);

impl<T: AsRef<[u8]>> From<T> for Memo {
    fn from(input: T) -> Self {
        let input_bytes: &[u8] = input.as_ref();

        let mut full_bytes = [0; 512];

        if input_bytes.len() < 512 {
            full_bytes[0..input_bytes.len()].copy_from_slice(input_bytes);
            Memo(full_bytes)
        } else if input_bytes.len() == 512 {
            full_bytes[..].copy_from_slice(input_bytes);
            Memo(full_bytes)
        } else {
            // Because this is a From impl, we truncate your input
            // rather than return any Memo at all.
            full_bytes.copy_from_slice(&input_bytes[0..512]);
            Memo(full_bytes)
        }
    }
}

impl fmt::Debug for Memo {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let output: String;

        // This saves work but if the 'valid utf8 string' is just a
        // bunch of numbers, it prints them out like
        // 'Memo("\u{0}\u{0}..")', so. ¯\_(ツ)_/¯
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

    let mut some_bytes = [0u8; 512];
    some_bytes[0] = 0xF6;

    println!("{:?}", Memo(some_bytes));
}

#[test]
fn memo_from_string() {
    let memo = Memo::from("foo bar baz");

    let mut bytes = [0; 512];
    bytes[0..11].copy_from_slice(&[102, 111, 111, 32, 98, 97, 114, 32, 98, 97, 122]);

    assert!(memo.0.iter().eq(bytes.iter()));
}

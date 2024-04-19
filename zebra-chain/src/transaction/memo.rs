use std::{cmp, fmt};

/// A 512-byte (plaintext) memo associated with a note, as described in
/// [protocol specification §5.5][ps].
///
/// The _Memo_ field of a note is a plaintext type; the parent note is
/// what is encrypted and stored on the blockchain. The underlying
/// usage of the memo field is by agreement between the sender and
/// recipient of the note.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#notept
#[derive(Clone)]
pub struct Memo(pub(crate) Box<[u8; 512]>);

impl<'a> TryFrom<&'a [u8]> for Memo {
    type Error = &'static str;

    fn try_from(input: &'a [u8]) -> Result<Self, Self::Error> {
        let mut full_bytes = [0; 512];

        match input.len().cmp(&512) {
            cmp::Ordering::Less => {
                full_bytes[0..input.len()].copy_from_slice(input);
                Ok(Memo(Box::new(full_bytes)))
            }
            cmp::Ordering::Equal => {
                full_bytes[..].copy_from_slice(input);
                Ok(Memo(Box::new(full_bytes)))
            }
            cmp::Ordering::Greater => Err("Memos have a max length of 512 bytes."),
        }
    }
}

impl fmt::Debug for Memo {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // This saves work but if the 'valid utf8 string' is just a
        // bunch of numbers, it prints them out like
        // 'Memo("\u{0}\u{0}..")', so. ¯\_(ツ)_/¯
        let output: String = match std::str::from_utf8(&self.0[..]) {
            Ok(memo) => String::from(memo),
            _ => hex::encode(&self.0[..]),
        };

        f.debug_tuple("Memo").field(&output).finish()
    }
}

#[test]
fn memo_fmt() {
    let _init_guard = zebra_test::init();

    // Rust changed the escaping of ' between 1.52 and 1.53 (nightly-2021-04-14?),
    // so the memo string can't contain '
    //
    // TODO: rewrite this test so it doesn't depend on the exact debug format
    let memo = Memo(Box::new(
        *b"thiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiis \
           iiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiis \
           aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
           veeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeryyyyyyyyyyyyyyyyyyyyyyyyyy \
           looooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooong \
           meeeeeeeeeeeeeeeeeeemooooooooooooooooooooooooooooooooooooooooooooooooooooooooooo \
           but its just short enough!",
    ));

    assert_eq!(format!("{memo:?}"),
               "Memo(\"thiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiis iiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiis aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa veeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeryyyyyyyyyyyyyyyyyyyyyyyyyy looooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooong meeeeeeeeeeeeeeeeeeemooooooooooooooooooooooooooooooooooooooooooooooooooooooooooo but its just short enough!\")"
    );

    let mut some_bytes = [0u8; 512];
    some_bytes[0] = 0xF6;

    assert_eq!(format!("{:?}", Memo(Box::new(some_bytes))),
               "Memo(\"f600000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\")"
    );
}

#[test]
fn memo_from_string() {
    let _init_guard = zebra_test::init();

    let memo = Memo::try_from("foo bar baz".as_ref()).unwrap();

    let mut bytes = [0; 512];
    bytes[0..11].copy_from_slice(&[102, 111, 111, 32, 98, 97, 114, 32, 98, 97, 122]);

    assert!(memo.0.iter().eq(bytes.iter()));
}

use std::io;

use super::WriteZcashExt;

/// Consensus-critical serialization for Zcash.
///
/// This trait provides a generic serialization for consensus-critical
/// formats, such as network messages, transactions, blocks, etc. It is intended
/// for use only in consensus-critical contexts; in other contexts, such as
/// internal storage, it would be preferable to use Serde.
pub trait ZcashSerialize: Sized {
    /// Write `self` to the given `writer` using the canonical format.
    ///
    /// This function has a `zcash_` prefix to alert the reader that the
    /// serialization in use is consensus-critical serialization, rather than
    /// some other kind of serialization.
    ///
    /// Notice that the error type is [`std::io::Error`]; this indicates that
    /// serialization MUST be infallible up to errors in the underlying writer.
    /// In other words, any type implementing `ZcashSerialize` must make illegal
    /// states unrepresentable.
    fn zcash_serialize<W: io::Write>(&self, writer: W) -> Result<(), io::Error>;

    /// Helper function to construct a vec to serialize the current struct into
    fn zcash_serialize_to_vec(&self) -> Result<Vec<u8>, io::Error> {
        let mut data = Vec::new();
        self.zcash_serialize(&mut data)?;
        Ok(data)
    }
}

impl<T: ZcashSerialize> ZcashSerialize for Vec<T> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_compactsize(self.len() as u64)?;
        for x in self {
            x.zcash_serialize(&mut writer)?;
        }
        Ok(())
    }
}

/// The maximum length of a Zcash message, in bytes.
///
/// This value is used to calculate safe preallocation limits for some types
pub const MAX_PROTOCOL_MESSAGE_LEN: usize = 2 * 1024 * 1024;

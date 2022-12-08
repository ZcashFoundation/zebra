//! Zebra script opcodes.

/// Supported opcodes
///
/// <https://github.com/zcash/zcash/blob/8b16094f6672d8268ff25b2d7bddd6a6207873f7/src/script/script.h#L39>
pub enum OpCode {
    // Opcodes used to generate P2SH scripts.
    Equal = 0x87,
    Hash160 = 0xa9,
    Push20Bytes = 0x14,
    // Additional opcodes used to generate P2PKH scripts.
    Dup = 0x76,
    EqualVerify = 0x88,
    CheckSig = 0xac,
}

//! Zebra script opcodes.

/// Supported opcodes
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

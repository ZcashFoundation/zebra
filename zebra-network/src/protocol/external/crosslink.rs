//! Crosslink BFT consensus network message types.
//!
//! These types define the wire format for BFT consensus messages exchanged between
//! Crosslink validator peers, replacing the previous protobuf-based encoding.
//! All types use `ZcashSerialize`/`ZcashDeserialize` for encoding.

use std::io::{Read, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use zebra_chain::crosslink::{BftBlock, BftBlockAndFatPointerToIt, BftVote};
use zebra_chain::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

/// A BFT block proposal from a validator.
///
/// Sent by the current round's proposer to all validators.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrosslinkProposal {
    /// BFT consensus height
    pub height: u64,
    /// BFT consensus round within this height
    pub round: u32,
    /// The proposed BFT block
    pub block: BftBlock,
    /// Proof-of-lock round, if any (-1 means none)
    pub pol_round: i32,
    /// Proposer's ed25519 public key
    pub proposer: [u8; 32],
    /// ed25519 signature over the proposal
    pub signature: [u8; 64],
}

impl ZcashSerialize for CrosslinkProposal {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_u64::<LittleEndian>(self.height)?;
        writer.write_u32::<LittleEndian>(self.round)?;
        self.block.zcash_serialize(&mut writer)?;
        writer.write_i32::<LittleEndian>(self.pol_round)?;
        writer.write_all(&self.proposer)?;
        writer.write_all(&self.signature)?;
        Ok(())
    }
}

impl ZcashDeserialize for CrosslinkProposal {
    fn zcash_deserialize<R: Read>(mut reader: R) -> Result<Self, SerializationError> {
        let height = reader.read_u64::<LittleEndian>()?;
        let round = reader.read_u32::<LittleEndian>()?;
        let block = BftBlock::zcash_deserialize(&mut reader)?;
        let pol_round = reader.read_i32::<LittleEndian>()?;
        let mut proposer = [0u8; 32];
        reader.read_exact(&mut proposer)?;
        let mut signature = [0u8; 64];
        reader.read_exact(&mut signature)?;
        Ok(CrosslinkProposal {
            height,
            round,
            block,
            pol_round,
            proposer,
            signature,
        })
    }
}

/// A BFT vote (prevote or precommit) from a validator.
///
/// Contains the 76-byte vote and the 64-byte ed25519 signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrosslinkVote {
    /// The BFT vote (76 bytes: pubkey + value hash + height + round/type)
    pub vote: BftVote,
    /// ed25519 signature over the vote bytes
    pub signature: [u8; 64],
}

impl ZcashSerialize for CrosslinkVote {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_all(&self.vote.to_bytes())?;
        writer.write_all(&self.signature)?;
        Ok(())
    }
}

impl ZcashDeserialize for CrosslinkVote {
    fn zcash_deserialize<R: Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut vote_bytes = [0u8; 76];
        reader.read_exact(&mut vote_bytes)?;
        let mut signature = [0u8; 64];
        reader.read_exact(&mut signature)?;
        Ok(CrosslinkVote {
            vote: BftVote::from_bytes(&vote_bytes),
            signature,
        })
    }
}

/// A decided BFT block with its commit certificate (fat pointer).
///
/// Broadcast after a BFT round reaches 2f+1 precommits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrosslinkDecided {
    /// The decided BFT block and its fat pointer proving consensus
    pub decided: BftBlockAndFatPointerToIt,
}

impl ZcashSerialize for CrosslinkDecided {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        self.decided.zcash_serialize(&mut writer)
    }
}

impl ZcashDeserialize for CrosslinkDecided {
    fn zcash_deserialize<R: Read>(reader: R) -> Result<Self, SerializationError> {
        Ok(CrosslinkDecided {
            decided: BftBlockAndFatPointerToIt::zcash_deserialize(reader)?,
        })
    }
}

/// BFT peer status announcement.
///
/// Periodically exchanged between BFT peers so they know each other's chain height.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrosslinkStatus {
    /// The peer's BFT chain tip height
    pub bft_tip_height: u64,
    /// The earliest BFT height this peer has available
    pub earliest_height: u64,
}

impl ZcashSerialize for CrosslinkStatus {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_u64::<LittleEndian>(self.bft_tip_height)?;
        writer.write_u64::<LittleEndian>(self.earliest_height)?;
        Ok(())
    }
}

impl ZcashDeserialize for CrosslinkStatus {
    fn zcash_deserialize<R: Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(CrosslinkStatus {
            bft_tip_height: reader.read_u64::<LittleEndian>()?,
            earliest_height: reader.read_u64::<LittleEndian>()?,
        })
    }
}

/// Request a decided BFT block at a specific height for sync.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrosslinkSyncRequest {
    /// The BFT height to request
    pub height: u64,
}

impl ZcashSerialize for CrosslinkSyncRequest {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_u64::<LittleEndian>(self.height)?;
        Ok(())
    }
}

impl ZcashDeserialize for CrosslinkSyncRequest {
    fn zcash_deserialize<R: Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(CrosslinkSyncRequest {
            height: reader.read_u64::<LittleEndian>()?,
        })
    }
}

/// Response with a decided BFT block and its commit certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrosslinkSyncResponse {
    /// The requested BFT height (echoed back for correlation)
    pub height: u64,
    /// The decided block and its fat pointer
    pub decided: BftBlockAndFatPointerToIt,
}

impl ZcashSerialize for CrosslinkSyncResponse {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_u64::<LittleEndian>(self.height)?;
        self.decided.zcash_serialize(&mut writer)?;
        Ok(())
    }
}

impl ZcashDeserialize for CrosslinkSyncResponse {
    fn zcash_deserialize<R: Read>(mut reader: R) -> Result<Self, SerializationError> {
        let height = reader.read_u64::<LittleEndian>()?;
        let decided = BftBlockAndFatPointerToIt::zcash_deserialize(&mut reader)?;
        Ok(CrosslinkSyncResponse { height, decided })
    }
}

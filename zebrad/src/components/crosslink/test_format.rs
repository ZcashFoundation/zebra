#![allow(unused, missing_docs)]

use static_assertions::*;
use std::{io::Write, mem::align_of, mem::size_of, sync::Arc, time::Duration};
use tracing::error;
use zerocopy::*;
use zerocopy_derive::*;

use zebra_chain::block::{Block, FatPointerToBftBlock, Hash as BlockHash};
use zebra_chain::crosslink::*;
use zebra_chain::serialization::{ZcashDeserialize, ZcashSerialize};
use zebra_state::{crosslink::*, Request as StateRequest, Response as StateResponse};

use ed25519_zebra::VerificationKeyBytes as MalPublicKey;

use std::sync::atomic::Ordering;

use super::{
    block_height_from_hash, tfl_block_finality_from_height_hash, TFLServiceInternal,
    TEST_CHECK_ASSERT, TEST_FAILED_INSTR_IDXS, TEST_INSTR_BYTES, TEST_INSTR_C, TEST_INSTR_PATH,
    TEST_INSTRS, TEST_NAME, TEST_SHUTDOWN_FN,
};
use super::service::TFLServiceHandle;

#[repr(C)]
#[derive(Immutable, KnownLayout, IntoBytes, FromBytes)]
pub struct TFHdr {
    pub magic: [u8; 8],
    pub instrs_o: u64,
    pub instrs_n: u32,
    pub instr_size: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Immutable, IntoBytes, FromBytes)]
pub struct TFSlice {
    pub o: u64,
    pub size: u64,
}

impl TFSlice {
    pub fn as_val(self) -> [u64; 2] {
        [self.o, self.size]
    }

    pub fn as_byte_slice_in(self, bytes: &[u8]) -> &[u8] {
        &bytes[self.o as usize..(self.o + self.size) as usize]
    }
}

impl From<&[u64; 2]> for TFSlice {
    fn from(val: &[u64; 2]) -> TFSlice {
        TFSlice {
            o: val[0],
            size: val[1],
        }
    }
}

type TFInstrKind = u32;

#[repr(C)]
#[derive(Clone, Copy, Immutable, IntoBytes, FromBytes)]
pub struct TFInstr {
    pub kind: TFInstrKind,
    pub flags: u32,
    pub data: TFSlice,
    pub val: [u64; 2],
}

pub const TEST_STAKE_IGNORED: u64 = u64::MAX;

static TF_INSTR_KIND_STRS: [&str; TFInstr::COUNT as usize] = {
    let mut strs = [""; TFInstr::COUNT as usize];
    strs[TFInstr::LOAD_POW as usize] = "LOAD_POW";
    strs[TFInstr::LOAD_POS as usize] = "LOAD_POS";
    strs[TFInstr::SET_PARAMS as usize] = "SET_PARAMS";
    strs[TFInstr::EXPECT_POW_CHAIN_LENGTH as usize] = "EXPECT_POW_CHAIN_LENGTH";
    strs[TFInstr::EXPECT_POS_CHAIN_LENGTH as usize] = "EXPECT_POS_CHAIN_LENGTH";
    strs[TFInstr::EXPECT_POW_BLOCK_FINALITY as usize] = "EXPECT_POW_BLOCK_FINALITY";
    strs[TFInstr::ROSTER_FORCE_INCLUDE as usize] = "ROSTER_FORCE_INCLUDE";
    strs[TFInstr::EXPECT_ROSTER_INCLUDES as usize] = "EXPECT_ROSTER_INCLUDES";

    const_assert!(TFInstr::COUNT == 8);
    strs
};

impl TFInstr {
    pub const LOAD_POW: TFInstrKind = 0;
    pub const LOAD_POS: TFInstrKind = 1;
    pub const SET_PARAMS: TFInstrKind = 2;
    pub const EXPECT_POW_CHAIN_LENGTH: TFInstrKind = 3;
    pub const EXPECT_POS_CHAIN_LENGTH: TFInstrKind = 4;
    pub const EXPECT_POW_BLOCK_FINALITY: TFInstrKind = 5;
    pub const ROSTER_FORCE_INCLUDE: TFInstrKind = 6;
    pub const EXPECT_ROSTER_INCLUDES: TFInstrKind = 7;
    pub const COUNT: TFInstrKind = 8;

    pub fn str_from_kind(kind: TFInstrKind) -> &'static str {
        let kind = kind as usize;
        if kind < TF_INSTR_KIND_STRS.len() {
            TF_INSTR_KIND_STRS[kind]
        } else {
            "<unknown>"
        }
    }

    pub fn string_from_instr(bytes: &[u8], instr: &TFInstr) -> String {
        let mut str = Self::str_from_kind(instr.kind).to_string();
        str += " (";

        match tf_read_instr(bytes, instr) {
            Some(TestInstr::LoadPoW(block)) => {
                str += &format!(
                    "{} - {}, parent: {}",
                    block.coinbase_height().unwrap().0,
                    block.hash(),
                    block.header.previous_block_hash
                )
            }
            Some(TestInstr::LoadPoS((block, _fat_ptr))) => {
                str += &format!(
                    "{}, hdrs: [{} .. {}]",
                    block.blake3_hash(),
                    block.headers[0].hash(),
                    block.headers.last().unwrap().hash()
                )
            }
            Some(TestInstr::SetParams(_)) => str += &format!("{} {}", instr.val[0], instr.val[1]),
            Some(TestInstr::ExpectPoWChainLength(h)) => str += &h.to_string(),
            Some(TestInstr::ExpectPoSChainLength(h)) => str += &h.to_string(),
            Some(TestInstr::ExpectPoWBlockFinality(hash, f)) => {
                str += &format!("{} => {:?}", hash, f)
            }
            Some(TestInstr::ExpectRosterIncludes(pub_key, stake)) => {
                str += &format!("{} => {}", BftValidatorAddress(pub_key.into()), stake)
            }
            Some(TestInstr::RosterForceInclude(pub_key, stake)) => {
                str += &format!("{} => {}", BftValidatorAddress(pub_key.into()), stake)
            }
            None => {}
        }

        str += ")";

        if instr.flags != 0 {
            str += " [";
            if (instr.flags & SHOULD_FAIL) != 0 {
                str += " SHOULD_FAIL";
            }
            str += " ]";
        }

        str
    }

    pub fn data_slice<'a>(&self, bytes: &'a [u8]) -> &'a [u8] {
        self.data.as_byte_slice_in(bytes)
    }
}

// Flags
pub const SHOULD_FAIL: u32 = 1 << 0;

pub struct TF {
    pub instrs: Vec<TFInstr>,
    pub data: Vec<u8>,
}

pub const TF_NOT_YET_FINALIZED: u64 = 0;
pub const TF_FINALIZED: u64 = 1;
pub const TF_CANT_BE_FINALIZED: u64 = 2;

pub fn finality_from_val(val: &[u64; 2]) -> Option<TFLBlockFinality> {
    if val[0] == 0 {
        None
    } else {
        match val[1] {
            TF_NOT_YET_FINALIZED => Some(TFLBlockFinality::NotYetFinalized),
            TF_FINALIZED => Some(TFLBlockFinality::Finalized),
            TF_CANT_BE_FINALIZED => Some(TFLBlockFinality::CantBeFinalized),
            _ => panic!("unexpected finality value"),
        }
    }
}

pub fn val_from_finality(val: Option<TFLBlockFinality>) -> [u64; 2] {
    match val {
        Some(TFLBlockFinality::NotYetFinalized) => [1u64, TF_NOT_YET_FINALIZED],
        Some(TFLBlockFinality::Finalized) => [1u64, TF_FINALIZED],
        Some(TFLBlockFinality::CantBeFinalized) => [1u64, TF_CANT_BE_FINALIZED],
        None => [0u64; 2],
    }
}

impl TF {
    pub fn new(params: &ZcashCrosslinkParameters) -> TF {
        let tf = TF {
            instrs: Vec::new(),
            data: Vec::new(),
        };
        tf
    }

    pub fn push_serialize<Z: ZcashSerialize>(&mut self, z: &Z) -> TFSlice {
        let bgn = (size_of::<TFHdr>() + self.data.len()) as u64;
        z.zcash_serialize(&mut self.data).ok();
        let end = (size_of::<TFHdr>() + self.data.len()) as u64;
        TFSlice {
            o: bgn,
            size: end - bgn,
        }
    }

    pub fn push_data(&mut self, bytes: &[u8]) -> TFSlice {
        let result = TFSlice {
            o: (size_of::<TFHdr>() + self.data.len()) as u64,
            size: bytes.len() as u64,
        };
        self.data.write_all(bytes).ok();
        result
    }

    pub fn push_instr_ex(&mut self, kind: TFInstrKind, flags: u32, data: &[u8], val: [u64; 2]) {
        let data = self.push_data(data);
        self.instrs.push(TFInstr {
            kind,
            flags,
            data,
            val,
        });
    }

    pub fn push_instr(&mut self, kind: TFInstrKind, data: &[u8]) {
        self.push_instr_ex(kind, 0, data, [0; 2])
    }

    pub fn push_instr_val(&mut self, kind: TFInstrKind, val: [u64; 2]) {
        self.push_instr_ex(kind, 0, &[], val)
    }

    pub fn push_instr_serialize_ex<Z: ZcashSerialize>(
        &mut self,
        kind: TFInstrKind,
        flags: u32,
        data: &Z,
        val: [u64; 2],
    ) {
        let data = self.push_serialize(data);
        self.instrs.push(TFInstr {
            kind,
            flags,
            data,
            val,
        });
    }

    pub fn push_instr_serialize<Z: ZcashSerialize>(&mut self, kind: TFInstrKind, data: &Z) {
        self.push_instr_serialize_ex(kind, 0, data, [0; 2])
    }

    pub fn push_instr_load_pow(&mut self, data: &Block, flags: u32) {
        self.push_instr_serialize_ex(TFInstr::LOAD_POW, flags, data, [0; 2])
    }

    pub fn push_instr_load_pow_bytes(&mut self, data: &[u8], flags: u32) {
        self.push_instr_ex(TFInstr::LOAD_POW, flags, data, [0; 2])
    }

    pub fn push_instr_load_pos(&mut self, data: &BftBlockAndFatPointerToIt, flags: u32) {
        self.push_instr_serialize_ex(TFInstr::LOAD_POS, flags, data, [0; 2])
    }

    pub fn push_instr_load_pos_bytes(&mut self, data: &[u8], flags: u32) {
        self.push_instr_ex(TFInstr::LOAD_POS, flags, data, [0; 2])
    }

    pub fn push_instr_expect_pow_chain_length(&mut self, length: usize, flags: u32) {
        self.push_instr_ex(
            TFInstr::EXPECT_POW_CHAIN_LENGTH,
            flags,
            &[],
            [length as u64, 0],
        )
    }

    pub fn push_instr_expect_pos_chain_length(&mut self, length: usize, flags: u32) {
        self.push_instr_ex(
            TFInstr::EXPECT_POS_CHAIN_LENGTH,
            flags,
            &[],
            [length as u64, 0],
        )
    }

    pub fn push_instr_expect_pow_block_finality(
        &mut self,
        pow_hash: &BlockHash,
        finality: Option<TFLBlockFinality>,
        flags: u32,
    ) {
        self.push_instr_ex(
            TFInstr::EXPECT_POW_BLOCK_FINALITY,
            flags,
            &pow_hash.0,
            val_from_finality(finality),
        )
    }

    pub fn push_instr_roster_force_include(&mut self, pub_key: [u8; 32], stake: u64, flags: u32) {
        self.push_instr_ex(TFInstr::ROSTER_FORCE_INCLUDE, flags, &pub_key, [stake, 0])
    }

    pub fn push_instr_expect_roster_includes(
        &mut self,
        pub_key: [u8; 32],
        stake: u64,
        flags: u32,
    ) {
        self.push_instr_ex(TFInstr::EXPECT_ROSTER_INCLUDES, flags, &pub_key, [stake, 0])
    }

    fn is_a_power_of_2(v: usize) -> bool {
        v != 0 && ((v & (v - 1)) == 0)
    }

    fn align_up(v: usize, mut align: usize) -> usize {
        assert!(Self::is_a_power_of_2(align));
        align -= 1;
        (v + align) & !align
    }

    pub fn write<W: std::io::Write>(&self, writer: &mut W) -> bool {
        let instrs_o_unaligned = size_of::<TFHdr>() + self.data.len();
        let instrs_o = Self::align_up(instrs_o_unaligned, align_of::<TFInstr>());
        let hdr = TFHdr {
            magic: "ZECCLTF0".as_bytes().try_into().unwrap(),
            instrs_o: instrs_o as u64,
            instrs_n: self.instrs.len() as u32,
            instr_size: size_of::<TFInstr>() as u32,
        };
        writer.write_all(hdr.as_bytes()).expect("writing shouldn't fail");
        writer.write_all(&self.data).expect("writing shouldn't fail");

        if instrs_o > instrs_o_unaligned {
            const ALIGN_0S: [u8; align_of::<TFInstr>()] = [0u8; align_of::<TFInstr>()];
            let align_size = instrs_o - instrs_o_unaligned;
            writer.write_all(&ALIGN_0S[..align_size]).ok();
        }
        writer.write_all(self.instrs.as_bytes()).expect("writing shouldn't fail");
        true
    }

    pub fn write_to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        self.write(&mut bytes);
        bytes
    }

    pub fn read_from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let tf_hdr = match TFHdr::ref_from_prefix(&bytes[0..]) {
            Ok((hdr, _)) => hdr,
            Err(err) => return Err(err.to_string()),
        };

        let read_instrs = <[TFInstr]>::ref_from_prefix_with_elems(
            &bytes[tf_hdr.instrs_o as usize..],
            tf_hdr.instrs_n as usize,
        );

        let instrs = match read_instrs {
            Ok((instrs, _)) => instrs,
            Err(err) => return Err(err.to_string()),
        };

        let data = &bytes[size_of::<TFHdr>()..tf_hdr.instrs_o as usize];

        Ok(TF {
            instrs: instrs.to_vec(),
            data: data.to_vec(),
        })
    }

    pub fn read_from_file(path: &std::path::Path) -> Result<(Vec<u8>, Self), String> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) => return Err(err.to_string()),
        };
        Self::read_from_bytes(&bytes).map(|tf| (bytes, tf))
    }
}

fn test_check(flags: u32, condition: bool, message: &str) {
    let should_succeed = (flags & SHOULD_FAIL) == 0;
    const SUCCESS_STRS: [&str; 2] = ["fail", "succeed"];

    if condition != should_succeed {
        let test_instr_i = TEST_INSTR_C.load(Ordering::Relaxed);
        TEST_FAILED_INSTR_IDXS.lock().unwrap().push(test_instr_i);

        if TEST_CHECK_ASSERT.load(Ordering::Relaxed) {
            panic!(
                "test check should {} but actually {}ed, message:\n{}",
                SUCCESS_STRS[should_succeed as usize],
                SUCCESS_STRS[!should_succeed as usize],
                message
            );
        } else {
            error!(
                "test check should {} but actually {}ed, message:\n{}",
                SUCCESS_STRS[should_succeed as usize],
                SUCCESS_STRS[!should_succeed as usize],
                message
            );
        }
    }
}

pub(crate) fn tf_read_instr(bytes: &[u8], instr: &TFInstr) -> Option<TestInstr> {
    const_assert!(TFInstr::COUNT == 8);
    match instr.kind {
        TFInstr::LOAD_POW => {
            let block = Block::zcash_deserialize(instr.data_slice(bytes)).ok()?;
            Some(TestInstr::LoadPoW(block))
        }

        TFInstr::LOAD_POS => {
            let block_and_fat_ptr =
                BftBlockAndFatPointerToIt::zcash_deserialize(instr.data_slice(bytes)).ok()?;
            Some(TestInstr::LoadPoS((
                block_and_fat_ptr.block,
                block_and_fat_ptr.fat_ptr,
            )))
        }

        TFInstr::SET_PARAMS => Some(TestInstr::SetParams(ZcashCrosslinkParameters {
            bc_confirmation_depth_sigma: instr.val[0],
            finalization_gap_bound: instr.val[1],
        })),

        TFInstr::EXPECT_POW_CHAIN_LENGTH => {
            Some(TestInstr::ExpectPoWChainLength(instr.val[0] as u32))
        }
        TFInstr::EXPECT_POS_CHAIN_LENGTH => Some(TestInstr::ExpectPoSChainLength(instr.val[0])),

        TFInstr::EXPECT_POW_BLOCK_FINALITY => Some(TestInstr::ExpectPoWBlockFinality(
            BlockHash(
                instr
                    .data_slice(bytes)
                    .try_into()
                    .expect("should be 32 bytes for hash"),
            ),
            finality_from_val(&instr.val),
        )),

        TFInstr::ROSTER_FORCE_INCLUDE => Some(TestInstr::RosterForceInclude(
            instr.data_slice(bytes).try_into().expect("32-byte array"),
            instr.val[0],
        )),
        TFInstr::EXPECT_ROSTER_INCLUDES => Some(TestInstr::ExpectRosterIncludes(
            instr.data_slice(bytes).try_into().expect("32-byte array"),
            instr.val[0],
        )),

        _ => {
            panic!("Unrecognized instruction {}", instr.kind);
        }
    }
}

#[derive(Clone)]
pub(crate) enum TestInstr {
    LoadPoW(Block),
    LoadPoS((BftBlock, FatPointerToBftBlock)),
    SetParams(ZcashCrosslinkParameters),
    ExpectPoWChainLength(u32),
    ExpectPoSChainLength(u64),
    ExpectPoWBlockFinality(BlockHash, Option<TFLBlockFinality>),
    RosterForceInclude([u8; 32], u64),
    ExpectRosterIncludes([u8; 32], u64),
}

pub(crate) async fn handle_instr(
    internal_handle: &TFLServiceHandle,
    bytes: &[u8],
    instr: TestInstr,
    flags: u32,
    instr_i: usize,
) {
    match instr {
        TestInstr::LoadPoW(block) => {
            let force_feed_ok = (internal_handle.call.force_feed_pow)(Arc::new(block)).await;
            test_check(flags, force_feed_ok, "PoW force feed ok");
        }

        TestInstr::LoadPoS((block, fat_ptr)) => {
            let force_feed_ok =
                (internal_handle.call.force_feed_pos)(Arc::new(block), fat_ptr).await;
            test_check(flags, force_feed_ok, "PoS force feed ok");
        }

        TestInstr::SetParams(_) => {
            debug_assert!(instr_i == 0, "should only be set at the beginning");
            todo!("Params");
        }

        TestInstr::ExpectPoWChainLength(h) => {
            if let StateResponse::Tip(Some((height, _hash))) =
                (internal_handle.call.state)(StateRequest::Tip)
                    .await
                    .expect("can read tip")
            {
                let expect = h;
                let actual = height.0 + 1;
                test_check(
                    flags,
                    expect == actual,
                    &format!("PoW chain length: expected {}, actually {}", expect, actual),
                );
            }
        }

        TestInstr::ExpectPoSChainLength(h) => {
            let expect = h as usize;
            let actual = internal_handle.internal.lock().await.bft_blocks.len();
            test_check(
                flags,
                expect == actual,
                &format!("PoS chain length: expected {}, actually {}", expect, actual),
            );
        }

        TestInstr::ExpectPoWBlockFinality(hash, f) => {
            let expect = f;
            let height =
                block_height_from_hash(&internal_handle.call.clone(), hash).await;
            let actual = if let Some(height) = height {
                tfl_block_finality_from_height_hash(internal_handle.clone(), height, hash).await
            } else {
                Ok(None)
            }
            .expect("valid response, even if None");
            test_check(
                flags,
                expect == actual,
                &format!(
                    "PoW block finality at hash={}, height={:?}: expected {:?}, actually {:?}",
                    hash, height, expect, actual
                ),
            );
        }

        TestInstr::ExpectRosterIncludes(pub_key, stake) => {
            let key: MalPublicKey = pub_key.into();
            let internal = internal_handle.internal.lock().await;
            let finalizer = internal
                .validators_at_current_height
                .iter()
                .find(|x| x.public_key == key);

            if let Some(finalizer) = finalizer {
                test_check(
                    flags,
                    stake == TEST_STAKE_IGNORED || stake == finalizer.voting_power,
                    &format!(
                        "Finalizer stake: expected {}, actually {}",
                        stake, finalizer.voting_power
                    ),
                );
            } else {
                test_check(
                    flags,
                    false,
                    &format!(
                        "Finalizer found: {:?}",
                        BftValidatorAddress(pub_key.into())
                    ),
                );
            }
        }

        TestInstr::RosterForceInclude(pub_key, stake) => {
            let mut internal = internal_handle.internal.lock().await;
            internal
                .validators_at_current_height
                .push(BftValidator::new(pub_key.into(), stake));
        }
    }
}

pub async fn read_instrs(internal_handle: TFLServiceHandle, bytes: &[u8], instrs: &[TFInstr]) {
    for instr_i in 0..instrs.len() {
        let instr_val = &instrs[instr_i];

        if let Some(instr) = tf_read_instr(bytes, &instrs[instr_i]) {
            handle_instr(&internal_handle, bytes, instr, instrs[instr_i].flags, instr_i).await;
        } else {
            panic!("Failed to read {}", TFInstr::str_from_kind(instr_val.kind));
        }

        TEST_INSTR_C.store(instr_i + 1, Ordering::Relaxed);
    }
}

pub(crate) async fn instr_reader(internal_handle: TFLServiceHandle) {
    let call = internal_handle.call.clone();
    println!("waiting for tip before starting the test...");
    let before_time = tokio::time::Instant::now();
    loop {
        if let Ok(StateResponse::Tip(Some(_))) = (call.state)(StateRequest::Tip).await {
            break;
        } else {
            if before_time.elapsed().as_secs() > 30 {
                panic!("Timeout waiting for test to start.");
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    println!("Starting test!");

    let bytes = TEST_INSTR_BYTES.get_or_init(|| {
        if let Some(path) = TEST_INSTR_PATH.get() {
            match std::fs::read(path) {
                Ok(bytes) => bytes,
                Err(err) => panic!("Invalid test file: {:?}: {}", path, err),
            }
        } else {
            panic!("Neither TEST_INSTR_BYTES nor TEST_INSTR_PATH was set")
        }
    });

    let tf = match TF::read_from_bytes(bytes) {
        Ok(tf) => tf,
        Err(err) => panic!("Invalid test data: {}", err),
    };

    let _ = TEST_INSTRS.set(tf.instrs.clone());

    read_instrs(internal_handle, bytes, &tf.instrs).await;

    assert_eq!(
        TEST_INSTR_C.load(Ordering::Relaxed),
        tf.instrs.len(),
        "didn't complete test {}",
        TEST_NAME.get().copied().unwrap_or("‰‰TEST_NAME_NOT_SET‰‰")
    );
    assert!(
        TEST_FAILED_INSTR_IDXS.lock().unwrap().is_empty(),
        "failed test {}",
        TEST_NAME.get().copied().unwrap_or("‰‰TEST_NAME_NOT_SET‰‰")
    );
    println!("Test done, shutting down");
    if let Some(shutdown_fn) = TEST_SHUTDOWN_FN.get() {
        shutdown_fn();
    }
}

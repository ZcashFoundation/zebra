use lazy_static::lazy_static;

pub struct TestVector {
    pub(crate) domain: Vec<u8>,
    pub(crate) msg: Vec<bool>,
    pub(crate) hash: [u8; 32],
}

// From https://github.com/zcash/zcash-test-vectors/blob/master/test-vectors/rust/orchard_sinsemilla.rs
lazy_static! {
    pub static ref SINSEMILLA: [TestVector; 11] = [
        TestVector {
            domain: vec![
                0x7a, 0x2e, 0x63, 0x61, 0x73, 0x68, 0x3a, 0x74, 0x65, 0x73, 0x74, 0x2d, 0x53, 0x69,
                0x6e, 0x73, 0x65, 0x6d, 0x69, 0x6c, 0x6c, 0x61,
            ],
            msg: vec![
                false, false, false, true, false, true, true, false, true, false, true, false,
                false, true, true, false, false, false, true, true, false, true, true, false,
                false, false, true, true, false, true, true, false, true, true, true, true, false,
                true, true, false,
            ],
            hash: [
                0x98, 0x54, 0xaa, 0x38, 0x43, 0x63, 0xb5, 0x70, 0x8e, 0x06, 0xb4, 0x19, 0xb6, 0x43,
                0x58, 0x68, 0x39, 0x65, 0x3f, 0xba, 0x5a, 0x78, 0x2d, 0x2d, 0xb1, 0x4c, 0xed, 0x13,
                0xc1, 0x9a, 0x83, 0x2b,
            ],
        },
        TestVector {
            domain: vec![
                0x7a, 0x2e, 0x63, 0x61, 0x73, 0x68, 0x3a, 0x74, 0x65, 0x73, 0x74, 0x2d, 0x53, 0x69,
                0x6e, 0x73, 0x65, 0x6d, 0x69, 0x6c, 0x6c, 0x61, 0x2d, 0x6c, 0x6f, 0x6e, 0x67, 0x65,
                0x72,
            ],
            msg: vec![
                true, true, false, true, false, false, true, false, true, false, false, false,
                false, true, false, true, false, false, true, false, true, true, true, false,
                false, false, false, true, true, false, false, true, false, true, false, true,
                false, true, true, true, false, false, false, true, false, true, true, true, false,
                true, false, true, false, true, true, true, false, true, true, true, true, true,
                true, true, false, true, false, false, true, true, true, false, true, true, true,
                true, false, false, true, false, true, false, false, false, false, false, true,
                false, true, false, false, true, true, false, true, false, false, false, false,
                true, true, false, true, false, true, false, true, true, false, true, false, false,
                false, true, true, false, false, false, true, true, false, true,
            ],
            hash: [
                0xed, 0x5b, 0x98, 0x8e, 0x4e, 0x98, 0x17, 0x1f, 0x61, 0x8f, 0xee, 0xb1, 0x23, 0xe5,
                0xcd, 0x0d, 0xc2, 0xd3, 0x67, 0x11, 0xc5, 0x06, 0xd5, 0xbe, 0x11, 0x5c, 0xfe, 0x38,
                0x8f, 0x03, 0xc4, 0x00,
            ],
        },
        TestVector {
            domain: vec![
                0x7a, 0x2e, 0x63, 0x61, 0x73, 0x68, 0x3a, 0x74, 0x65, 0x73, 0x74, 0x2d, 0x53, 0x69,
                0x6e, 0x73, 0x65, 0x6d, 0x69, 0x6c, 0x6c, 0x61,
            ],
            msg: vec![
                true, false, true, true, false, true, false, true, false, true, false, true, true,
                true, true, true, true, true, false, true, false, true, true, false, true, false,
                true, false, true, false, true, false, false, true, false, true, true, false,
                false, false, true, true, false, true, false, false, true, true, true, true, true,
                false, true, true, false, false, true, false, false, false, true, true, false,
                false, false, false, true, false, false, true, true, false, false, true, false,
                false, true, false, true, true, false, true, true, false, false, true, true, true,
                true, true, false, false, false, true, false, false, true, false, false,
            ],
            hash: [
                0xd9, 0x5e, 0xe5, 0x8f, 0xbd, 0xaa, 0x6f, 0x3d, 0xe5, 0xe4, 0xfd, 0x7a, 0xfc, 0x35,
                0xfa, 0x9d, 0xcf, 0xe8, 0x2a, 0xd1, 0x93, 0x06, 0xb0, 0x7e, 0x6c, 0xda, 0x0c, 0x30,
                0xe5, 0x98, 0x34, 0x07,
            ],
        },
        TestVector {
            domain: vec![
                0x7a, 0x2e, 0x63, 0x61, 0x73, 0x68, 0x3a, 0x74, 0x65, 0x73, 0x74, 0x2d, 0x53, 0x69,
                0x6e, 0x73, 0x65, 0x6d, 0x69, 0x6c, 0x6c, 0x61,
            ],
            msg: vec![
                false, false, true, true, false, false, false, true, false, false, true, false,
                true, true, true, true, false, false, true, false, true, false, false, false, true,
                true, false, false, true, true, true, true, false, true, false, false, true, false,
                false, true, true, false, false, false, false, true, true, false, false, true,
                false, false, false, true, false, false, true, false, true, true, false, true,
                false, true, false, true, true, true, true, false, false, true, false, false, true,
                true, true, true, true, false, true, true, false, true, true, false, false, true,
                true, true, true, true, true, false, false, false, true, false, true, false, false,
                true, true, false, true, true, true, true, false, false, true, true, false, false,
                true, false, false, true, true, true, false, false, false, false, false, true,
                false, false, false, true, false, true, false, true, true, true, true, false, true,
                false, false, true, false, false, false, false, false, false, true, true, true,
                true, false, true, true, false, false, false, false, true, true, true, false, true,
                true, true, true, true, false, true, true, true, false, true, false, false, true,
                false, false, true, false, false, false, false, true, false, false, false, false,
                true, true, true, false, true, false, true, false, false, false, true, true, false,
                false, true, false, false, false, true, true, false, false,
            ],
            hash: [
                0x6a, 0x92, 0x4b, 0x41, 0x39, 0x84, 0x29, 0x91, 0x0a, 0x78, 0x83, 0x2b, 0x61, 0x19,
                0x2a, 0x0b, 0x67, 0x40, 0xd6, 0x27, 0x77, 0xeb, 0x71, 0x54, 0x50, 0x32, 0xeb, 0x6c,
                0xe9, 0x3e, 0xc9, 0x38,
            ],
        },
        TestVector {
            domain: vec![
                0x7a, 0x2e, 0x63, 0x61, 0x73, 0x68, 0x3a, 0x74, 0x65, 0x73, 0x74, 0x2d, 0x53, 0x69,
                0x6e, 0x73, 0x65, 0x6d, 0x69, 0x6c, 0x6c, 0x61, 0x2d, 0x6c, 0x6f, 0x6e, 0x67, 0x65,
                0x72,
            ],
            msg: vec![
                false, true, true, true, true, true, true, true, true, false, false, false, false,
                true, false, true, false, false, true, false, true, false, true, true, true, false,
                true, true, true, false, true, false, true, false, false, true, true, false, false,
                true, false, false, false, false, true, true, true, false, true, false, false,
                false, false, false, false, true, false, false, false, false, false,
            ],
            hash: [
                0xdc, 0x5f, 0xf0, 0x5b, 0x6f, 0x18, 0xb0, 0x76, 0xb6, 0x12, 0x82, 0x37, 0xa7, 0x59,
                0xed, 0xc7, 0xc8, 0x77, 0x8c, 0x70, 0x22, 0x2c, 0x79, 0xb7, 0x34, 0x03, 0x7b, 0x69,
                0x39, 0x3a, 0xbf, 0x3e,
            ],
        },
        TestVector {
            domain: vec![
                0x7a, 0x2e, 0x63, 0x61, 0x73, 0x68, 0x3a, 0x74, 0x65, 0x73, 0x74, 0x2d, 0x53, 0x69,
                0x6e, 0x73, 0x65, 0x6d, 0x69, 0x6c, 0x6c, 0x61,
            ],
            msg: vec![
                true, true, false, false, true, true, false, true, false, false, true, true, false,
                false, true, true, false, true, false, true, true, true, false, true, true, false,
                true, true, false, true, true, true, true, true, true, false, false, false, true,
                false, false, true, true, false, false, false, true, false, false, false, true,
                true, true, false, false, false, false, true, false, false, true, true, true, true,
                true, false, false, false, true, false, false, true, true, true, true, false, true,
                true, true, false, false, false, true, false, false, true, false, false, true,
                true, false, true, false, true, true, true, true, false, true, true, false, false,
                false, true, true, true, true, true, true, false, false, false, true, false, false,
                true, true, true, true, false, false, true, true, false, true, true, true, false,
                true, true, true, true, false, false, true, false, true, true, true, true, false,
                false, false, true, false, true, true, true, false, true, true, false, true, true,
                true, true, true, true, true, true, true, false, true, false, false, true, false,
                true, false, true, false, true, true, false, false, false, true, false, false,
                false, false, true, false, false, false, true, false, false, false, true, true,
                false, false, true, false, true, true, false, true, true, true, false, true, true,
                true, false, false, true, true,
            ],
            hash: [
                0xc7, 0x6c, 0x8d, 0x7c, 0x43, 0x55, 0x04, 0x1b, 0xd7, 0xa7, 0xc9, 0x9b, 0x54, 0x86,
                0x44, 0x19, 0x6f, 0x41, 0x94, 0x56, 0x20, 0x75, 0x37, 0xc2, 0x82, 0x85, 0x8a, 0x9b,
                0x19, 0x2d, 0x07, 0x3b,
            ],
        },
        TestVector {
            domain: vec![
                0x7a, 0x2e, 0x63, 0x61, 0x73, 0x68, 0x3a, 0x74, 0x65, 0x73, 0x74, 0x2d, 0x53, 0x69,
                0x6e, 0x73, 0x65, 0x6d, 0x69, 0x6c, 0x6c, 0x61, 0x2d, 0x6c, 0x6f, 0x6e, 0x67, 0x65,
                0x72,
            ],
            msg: vec![
                false, false, true, false, false, true, true, false, false, true, true, false,
                true, true, false, true, true, true, true, false, false, false, true, true, false,
                true, true, false, true, false, true, false, true, true, true, true, false, false,
                false, false, false, true, true, true, false, true, false, false, true, true, true,
                false, true, true, false, false, true, true, false, true, true, true, false, false,
                true, false, false, true, true, true, false, false, true, false, false, false,
                true, false, false, true, false, true, true, false, true, true, true, true, true,
                false, true, true, false, false, false, false, false, false, false, false,
            ],
            hash: [
                0x1a, 0xe8, 0x25, 0xeb, 0x42, 0xd7, 0x4e, 0x1b, 0xca, 0x7e, 0xe8, 0xa1, 0xf8, 0xf3,
                0xde, 0xd8, 0x01, 0xff, 0xcd, 0x1f, 0x22, 0xba, 0x75, 0xc3, 0x4b, 0xd6, 0xe0, 0x6a,
                0x2c, 0x7c, 0x5a, 0x20,
            ],
        },
        TestVector {
            domain: vec![
                0x7a, 0x2e, 0x63, 0x61, 0x73, 0x68, 0x3a, 0x74, 0x65, 0x73, 0x74, 0x2d, 0x53, 0x69,
                0x6e, 0x73, 0x65, 0x6d, 0x69, 0x6c, 0x6c, 0x61, 0x2d, 0x6c, 0x6f, 0x6e, 0x67, 0x65,
                0x72,
            ],
            msg: vec![
                true, true, false, true, true, true, false, false, true, false, false, false, true,
                true, false, false, true, true, true, false, false, true, false, true, true, true,
                true, false, true, true, true, false, false, true, true, false, true, true, true,
                true, true, false, true, true, true, true, true, false, true, true, false, true,
                true, true, false, true, false, true, false, false, true, false, true, true, true,
                true, true, false, true, true, false, true, true, false, false, false, true, false,
                true, true, false, false, false, true, false, false, false, false, true, false,
                false, true, false, false, false, false, true, false, false, false, false, false,
                true, true, true, false, false, false, true, true, false, false, false, false,
                true, false, false, true, true, true, false, true, false, false, true, true, false,
                false, false, true, false, false, true, false, false, false, false, false, true,
                false, false, true, true, true, false, false, true, false, false, false, true,
                false, false, false, true, false, false, false, false, true, false, true, false,
                true, false, false, false, false, true, true, false, false, false, true, true,
                true, true,
            ],
            hash: [
                0x38, 0xcf, 0xa6, 0x00, 0xaf, 0xd8, 0x67, 0x0e, 0x1f, 0x9a, 0x79, 0xcb, 0x22, 0x42,
                0x5f, 0xa9, 0x50, 0xcc, 0x4d, 0x3a, 0x3f, 0x5a, 0xfe, 0x39, 0x76, 0xd7, 0x1b, 0xb1,
                0x11, 0x46, 0x0c, 0x2b,
            ],
        },
        TestVector {
            domain: vec![
                0x7a, 0x2e, 0x63, 0x61, 0x73, 0x68, 0x3a, 0x74, 0x65, 0x73, 0x74, 0x2d, 0x53, 0x69,
                0x6e, 0x73, 0x65, 0x6d, 0x69, 0x6c, 0x6c, 0x61,
            ],
            msg: vec![
                false, false, true, false, true, false, true, false, false, true, true, false,
                true, true, true, true, true, false, false, true, true, false, true, false, true,
                true, true, false, false, false, false, true, false, false, true, true, false,
                false, false, false, true, true, true, true, true, true, true, true, false, false,
                true, false, true, false, false, false, false, true, false, true, true, true, true,
                true, true, false, true, true, true, true, false, true, false, true, false, false,
                false,
            ],
            hash: [
                0x82, 0x6f, 0xcb, 0xed, 0xfc, 0x83, 0xb9, 0xfa, 0xa5, 0x71, 0x1a, 0xab, 0x59, 0xbf,
                0xc9, 0x1b, 0xd4, 0x45, 0x58, 0x14, 0x67, 0x72, 0x5d, 0xde, 0x94, 0x1d, 0x58, 0xe6,
                0x26, 0x56, 0x66, 0x15,
            ],
        },
        TestVector {
            domain: vec![
                0x7a, 0x2e, 0x63, 0x61, 0x73, 0x68, 0x3a, 0x74, 0x65, 0x73, 0x74, 0x2d, 0x53, 0x69,
                0x6e, 0x73, 0x65, 0x6d, 0x69, 0x6c, 0x6c, 0x61,
            ],
            msg: vec![
                true, true, true, false, true, false, true, false, true, true, true, true, false,
                true, false, true, false, true, false, true, false, true, false, true, true, true,
                false, false, true, false, false, true, false, false, true, true, false, true,
                false, false, false, true, false, true, true, false, true, false, false, false,
                false, false, false, true, true, true, true, false, false, true, true, true, false,
                false, true, false, true, false, true, false, false, false, false, false, false,
                false, false, false, true, false, true, true, false, true, false, true, true, true,
                false, false, true, true, false, true, false, false, false, true, true, true,
                false, false, true, true, false, false, true, false, true, false, false, false,
                true, true, false,
            ],
            hash: [
                0x0b, 0xf0, 0x6c, 0xe8, 0x10, 0x05, 0xb8, 0x1a, 0x14, 0x80, 0x9f, 0xa6, 0xeb, 0xcb,
                0x94, 0xe2, 0xb6, 0x37, 0x5f, 0x87, 0xce, 0x51, 0x95, 0x8c, 0x94, 0x98, 0xed, 0x1a,
                0x31, 0x3c, 0x6a, 0x14,
            ],
        },
        TestVector {
            domain: vec![
                0x7a, 0x2e, 0x63, 0x61, 0x73, 0x68, 0x3a, 0x74, 0x65, 0x73, 0x74, 0x2d, 0x53, 0x69,
                0x6e, 0x73, 0x65, 0x6d, 0x69, 0x6c, 0x6c, 0x61,
            ],
            msg: vec![true, false, true, true, true, false, true, false],
            hash: [
                0x80, 0x6a, 0xcc, 0x24, 0x7a, 0xc9, 0xba, 0x90, 0xd2, 0x5f, 0x58, 0x3d, 0xad, 0xb5,
                0xe0, 0xee, 0x5c, 0x03, 0xe1, 0xab, 0x35, 0x70, 0xb3, 0x62, 0xb4, 0xbe, 0x5a, 0x8b,
                0xce, 0xb6, 0x0b, 0x00,
            ],
        },
    ];
}

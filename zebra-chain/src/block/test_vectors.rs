use std::fmt;

use hex::FromHex;

use lazy_static::lazy_static;

// Copied from librustzcash
// From mainnet block 415000.
// https://explorer.zcha.in/blocks/415000
pub const HEADER_MAINNET_415000_BYTES: [u8; 1487] = [
    0x04, 0x00, 0x00, 0x00, 0x52, 0x74, 0xb4, 0x3b, 0x9e, 0x4a, 0xd8, 0xf4, 0x3e, 0x93, 0xf7, 0x84,
    0x63, 0xd2, 0x4d, 0xcf, 0xe5, 0x31, 0xae, 0xb4, 0x71, 0x98, 0x19, 0xf4, 0xf9, 0x7f, 0x7e, 0x03,
    0x00, 0x00, 0x00, 0x00, 0x66, 0x30, 0x73, 0xbc, 0x4b, 0xfa, 0x95, 0xc9, 0xbe, 0xc3, 0x6a, 0xad,
    0x72, 0x68, 0xa5, 0x73, 0x04, 0x97, 0x97, 0xbd, 0xfc, 0x5a, 0xa4, 0xc7, 0x43, 0xfb, 0xe4, 0x82,
    0x0a, 0xa3, 0x93, 0xce, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0xa8, 0xbe, 0xcc, 0x5b, 0xe1, 0xab, 0x03, 0x1c, 0xc2, 0xfd, 0x60, 0x7c,
    0x77, 0x6a, 0x7a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3e, 0xb2, 0x18, 0x19, 0xfd, 0x40, 0x05, 0x00,
    0x94, 0x9d, 0x55, 0xde, 0x0c, 0xc6, 0x33, 0xe0, 0xcc, 0xe4, 0x1e, 0x46, 0x49, 0xef, 0x4a, 0xa3,
    0x34, 0x9f, 0x01, 0x00, 0x29, 0x0f, 0xfe, 0x28, 0x1b, 0x94, 0x7b, 0x3b, 0x53, 0xfb, 0xd2, 0xf3,
    0x5b, 0x1c, 0xe2, 0x92, 0x64, 0x9b, 0x96, 0xac, 0x6e, 0x08, 0x83, 0xaf, 0x3a, 0x68, 0x44, 0xb9,
    0x55, 0x92, 0xe7, 0x45, 0x56, 0xda, 0x34, 0x4b, 0x47, 0x01, 0x96, 0x1c, 0xd4, 0x13, 0x0c, 0x68,
    0x21, 0x9c, 0xfa, 0x13, 0x41, 0xd5, 0xaf, 0xb5, 0x04, 0x9e, 0xb0, 0xe8, 0xbe, 0x4a, 0x2d, 0x92,
    0xd6, 0x78, 0xc4, 0x07, 0x85, 0xe3, 0x37, 0x05, 0x54, 0x8b, 0x5f, 0x3a, 0x54, 0xf0, 0xa4, 0xc3,
    0x9a, 0x2f, 0x58, 0xee, 0x78, 0x4a, 0x24, 0x16, 0x3c, 0xd8, 0x6f, 0x54, 0x81, 0x23, 0x27, 0xdf,
    0x55, 0xe1, 0xd5, 0x5c, 0xa8, 0x4b, 0x6e, 0x7b, 0x88, 0x7a, 0x7c, 0xbf, 0xb9, 0x09, 0x1a, 0x58,
    0x5b, 0xdb, 0x8e, 0xa4, 0x75, 0x93, 0x07, 0xc5, 0x6c, 0x1b, 0x3d, 0xaf, 0xc6, 0x69, 0x24, 0x5a,
    0x6f, 0x65, 0x4b, 0x6f, 0x73, 0x00, 0x52, 0x26, 0x6a, 0x01, 0xad, 0x4f, 0x9c, 0x0b, 0x59, 0xed,
    0x4e, 0x17, 0x71, 0x2b, 0x3e, 0x72, 0xdf, 0x04, 0x98, 0xaa, 0x8d, 0xe4, 0x88, 0x8f, 0x99, 0x35,
    0x31, 0xc6, 0x0a, 0xcd, 0xed, 0x1d, 0x4b, 0x66, 0xe8, 0x9d, 0xe0, 0xb6, 0x48, 0x2c, 0xcc, 0xd4,
    0xa7, 0x12, 0xf5, 0xcf, 0x9d, 0x4c, 0xa8, 0x3b, 0xe0, 0xf9, 0x22, 0xde, 0x2c, 0x1d, 0xbb, 0x3a,
    0x14, 0x07, 0x48, 0x0d, 0xbe, 0x87, 0x95, 0x99, 0x3d, 0x8b, 0xe6, 0x40, 0x98, 0x8a, 0xbf, 0xe7,
    0xa8, 0xa1, 0xb3, 0x3a, 0x12, 0x13, 0x1c, 0x45, 0x1e, 0x1a, 0xbc, 0x0d, 0x83, 0xfb, 0x85, 0x18,
    0x62, 0xc6, 0x37, 0xce, 0x72, 0x4d, 0x5f, 0xe9, 0x7a, 0xa9, 0xa8, 0x06, 0xcf, 0x34, 0xba, 0xb5,
    0x09, 0xf4, 0x55, 0x4b, 0x0c, 0xd1, 0x0a, 0x7d, 0xdf, 0xd5, 0x82, 0x1b, 0x09, 0x1a, 0xd2, 0xc9,
    0x0c, 0x1a, 0xa1, 0xd8, 0x1e, 0xb3, 0xd7, 0x2d, 0xb4, 0x19, 0x93, 0xb6, 0x48, 0xf4, 0x1e, 0x21,
    0x38, 0xff, 0x95, 0x31, 0xa3, 0x0f, 0xf7, 0x3b, 0x22, 0x14, 0x0e, 0x4e, 0xbd, 0x7b, 0xaa, 0x33,
    0x84, 0x8e, 0x51, 0x2d, 0x99, 0x30, 0x0c, 0x5c, 0x13, 0x1c, 0x6e, 0x75, 0xf5, 0x71, 0x4a, 0x5c,
    0x6d, 0xcb, 0x17, 0x8b, 0x4a, 0x49, 0x78, 0xda, 0xc8, 0x3a, 0xd4, 0x12, 0xfb, 0xd6, 0x92, 0x01,
    0x92, 0x50, 0xc5, 0x53, 0x04, 0x9a, 0xad, 0x45, 0x79, 0x84, 0xbe, 0xdf, 0xc9, 0x6a, 0xe7, 0x01,
    0xc6, 0x59, 0xbc, 0x70, 0x07, 0xa9, 0x7d, 0x0a, 0x90, 0x02, 0xb9, 0x45, 0xbd, 0xec, 0x45, 0xa9,
    0x45, 0xef, 0x62, 0x85, 0xb2, 0xcd, 0x55, 0x3b, 0x4c, 0x09, 0xd9, 0x07, 0xc6, 0x27, 0x86, 0x3f,
    0x03, 0x99, 0xe8, 0x72, 0x5b, 0x4f, 0xf7, 0xfc, 0x59, 0x79, 0xe3, 0xcf, 0xf2, 0x28, 0x14, 0x50,
    0x84, 0x48, 0xef, 0x8b, 0x98, 0x31, 0xc2, 0x85, 0x95, 0x93, 0x33, 0x39, 0x6a, 0xa3, 0x62, 0xa5,
    0x1c, 0xf2, 0x05, 0x09, 0x7a, 0xfa, 0xbe, 0xc1, 0x5e, 0x41, 0xfb, 0x6e, 0x30, 0xb6, 0x22, 0x37,
    0x4b, 0xf5, 0x8b, 0x37, 0xef, 0x9d, 0x1b, 0x24, 0x1e, 0xad, 0x5a, 0x68, 0x2b, 0x98, 0xb6, 0x57,
    0x49, 0xa5, 0x75, 0x68, 0xe2, 0x38, 0xd5, 0x0a, 0xfd, 0x41, 0x7e, 0x1e, 0x96, 0x0e, 0x7b, 0x5a,
    0x06, 0x4f, 0xd9, 0xf6, 0x94, 0xd7, 0x83, 0xa2, 0xcb, 0xcd, 0x58, 0x55, 0x2d, 0xed, 0xbb, 0x9e,
    0x5e, 0x11, 0x23, 0x67, 0x4e, 0xf7, 0x3a, 0x52, 0x41, 0x96, 0xcf, 0x05, 0xd3, 0xe5, 0x24, 0x66,
    0x05, 0x49, 0xff, 0xe7, 0xbd, 0x65, 0x68, 0x05, 0x71, 0x35, 0xff, 0xd5, 0xaf, 0xd9, 0x43, 0xf6,
    0xda, 0x11, 0xcb, 0xb5, 0x97, 0xe8, 0xcc, 0xec, 0xd7, 0x7e, 0xcb, 0xe9, 0x09, 0xde, 0x06, 0x31,
    0xbf, 0xa2, 0x9c, 0xd3, 0xe3, 0xd5, 0x54, 0x46, 0x71, 0xba, 0x80, 0x25, 0x61, 0x53, 0xd6, 0xe9,
    0x99, 0x0b, 0x88, 0xad, 0x8e, 0x0c, 0xf4, 0x98, 0x9b, 0xef, 0x4b, 0xe4, 0x57, 0xf9, 0xc7, 0xb0,
    0xf1, 0xaa, 0xcd, 0x6e, 0x0e, 0xf3, 0x20, 0x60, 0x5c, 0x29, 0xed, 0x0c, 0xd2, 0xeb, 0x6c, 0xfc,
    0xe2, 0x16, 0xc5, 0x2a, 0x31, 0x75, 0x80, 0x20, 0x1c, 0xad, 0x7a, 0x09, 0x43, 0xd2, 0x4b, 0x7b,
    0x06, 0xd5, 0xbf, 0x75, 0x87, 0x61, 0xdd, 0x96, 0xe1, 0x19, 0x70, 0xb5, 0xde, 0xd6, 0x97, 0x22,
    0x2b, 0x2c, 0x77, 0xe7, 0xf2, 0x56, 0xa6, 0x05, 0xac, 0x75, 0x55, 0x49, 0xc1, 0x65, 0x1f, 0x25,
    0xad, 0xfc, 0x9d, 0x53, 0xd9, 0x11, 0x7e, 0x3a, 0x0b, 0xb4, 0x09, 0xee, 0xe4, 0xa6, 0x00, 0x12,
    0x04, 0x72, 0x94, 0x9c, 0x7d, 0xda, 0x1c, 0x2e, 0xdb, 0x3c, 0x33, 0x0c, 0x7f, 0x96, 0x17, 0x99,
    0x82, 0x91, 0x64, 0x57, 0xd3, 0x31, 0xe9, 0x63, 0x09, 0xdd, 0x24, 0xdf, 0x74, 0xee, 0xdd, 0x00,
    0xe7, 0xdb, 0x49, 0x7e, 0xe1, 0x30, 0xf7, 0x7d, 0xe6, 0x66, 0xeb, 0x55, 0x7f, 0xb3, 0x16, 0xe8,
    0x7a, 0xda, 0xf1, 0x81, 0x3c, 0xe4, 0x26, 0xa4, 0x58, 0xa6, 0xee, 0xe3, 0xa8, 0x5b, 0x2a, 0xb8,
    0x8f, 0x65, 0x53, 0xaa, 0xda, 0xe8, 0xde, 0x65, 0x2e, 0x21, 0x1a, 0x1d, 0x9f, 0x33, 0x4d, 0x59,
    0x6b, 0x5e, 0xb6, 0x17, 0x34, 0x07, 0xef, 0xcc, 0x2e, 0x81, 0x54, 0xbb, 0x9c, 0xa1, 0x21, 0x2a,
    0xa9, 0xa1, 0xa1, 0x12, 0x1d, 0x2f, 0x5a, 0x77, 0x12, 0xcf, 0x25, 0xcc, 0x81, 0x48, 0xb8, 0x05,
    0x2e, 0x0d, 0x2e, 0x09, 0xf2, 0x0e, 0x5b, 0xa2, 0xa9, 0x82, 0x77, 0xe9, 0x75, 0xb0, 0xee, 0xd9,
    0xa8, 0x92, 0x06, 0x96, 0x63, 0x37, 0x16, 0x3f, 0x21, 0x5c, 0x9d, 0x04, 0xa6, 0x59, 0x8b, 0x09,
    0x58, 0xd3, 0x33, 0xd8, 0x46, 0x77, 0x3c, 0x69, 0xe5, 0xab, 0xfd, 0x0a, 0x04, 0x27, 0xf3, 0x66,
    0x06, 0x14, 0xdd, 0x82, 0xb7, 0x9a, 0xdb, 0x85, 0x1a, 0x0d, 0x58, 0xb6, 0x2d, 0xf5, 0xf0, 0xb3,
    0xac, 0x83, 0x6e, 0x6e, 0x25, 0xf3, 0xa5, 0x1f, 0x49, 0xa9, 0x9a, 0xde, 0x57, 0x79, 0x6f, 0xe9,
    0xfc, 0xc2, 0x6f, 0x0a, 0x1f, 0x94, 0xff, 0x08, 0x19, 0xfe, 0x52, 0xb7, 0x50, 0x87, 0xed, 0xbe,
    0xd3, 0xa8, 0x16, 0x26, 0xeb, 0x54, 0x16, 0xc6, 0x65, 0x57, 0xf1, 0x1c, 0x0f, 0xce, 0xdf, 0xf2,
    0x23, 0xd6, 0xaa, 0x8c, 0xd5, 0xc3, 0x53, 0x86, 0xe5, 0xb4, 0xb9, 0x5a, 0x0f, 0x03, 0x92, 0xca,
    0x30, 0x1a, 0x38, 0xb3, 0x68, 0x7d, 0x09, 0x44, 0x93, 0xb9, 0xe9, 0xd2, 0x64, 0xd0, 0x7a, 0x19,
    0x0c, 0xe5, 0x7d, 0x11, 0x68, 0x04, 0x38, 0x2a, 0x3f, 0xab, 0xe1, 0x5a, 0xf4, 0xdf, 0x4f, 0xa0,
    0x43, 0xf0, 0x28, 0x7a, 0xa1, 0xed, 0x55, 0x68, 0xd9, 0xef, 0x5d, 0x12, 0x51, 0x0d, 0x01, 0x0c,
    0xcd, 0xab, 0x4e, 0xb6, 0x16, 0xf6, 0xdf, 0x13, 0xbb, 0x31, 0x26, 0xef, 0x43, 0xd9, 0xd6, 0x57,
    0x35, 0xe4, 0xe4, 0xc0, 0x4b, 0x57, 0x63, 0x48, 0xd0, 0x40, 0xb5, 0x35, 0x05, 0x5a, 0x3d, 0x5a,
    0xe1, 0x91, 0xb7, 0x5f, 0x06, 0x12, 0xf3, 0xb2, 0x40, 0x66, 0xa0, 0x52, 0x45, 0xf2, 0x7f, 0xe5,
    0x7b, 0xda, 0x66, 0xbd, 0x6d, 0xec, 0x7e, 0x4f, 0xc9, 0xcb, 0x23, 0x68, 0x02, 0x06, 0x2a, 0xdd,
    0xe3, 0xcd, 0x0e, 0x31, 0x34, 0x82, 0xc9, 0x2a, 0x0c, 0x72, 0x11, 0x02, 0xb1, 0xf3, 0x8b, 0x01,
    0x5a, 0xb8, 0xd0, 0x15, 0x59, 0xcb, 0xcb, 0x40, 0xf6, 0x74, 0xe9, 0xef, 0xad, 0x5e, 0xe9, 0xc2,
    0xfe, 0x13, 0x3f, 0xaa, 0x55, 0xca, 0x1d, 0xd0, 0xff, 0x26, 0x71, 0x0f, 0x9d, 0xa8, 0x19, 0xcc,
    0x14, 0x59, 0xcb, 0x7e, 0xd2, 0x60, 0xda, 0xd3, 0xdb, 0x05, 0x96, 0x25, 0x8d, 0x47, 0xc7, 0x4c,
    0x32, 0xa8, 0xb8, 0x52, 0xb6, 0x71, 0xc5, 0xa0, 0xca, 0xa2, 0x00, 0x16, 0x03, 0xd9, 0x0c, 0x91,
    0xa7, 0xdf, 0x2e, 0x2d, 0x4e, 0xe9, 0xae, 0x9b, 0xf1, 0xa6, 0xb1, 0xec, 0x88, 0x15, 0x1c, 0x62,
    0x36, 0x0d, 0x03, 0x02, 0x4d, 0x2e, 0x2d, 0x01, 0x14, 0x08, 0x4f, 0x6b, 0x88, 0xc5, 0xbb, 0xa2,
    0x4a, 0xa7, 0xce, 0xcf, 0xac, 0x16, 0xe9, 0x1e, 0x0b, 0xaf, 0x3d, 0x86, 0x53, 0xe2, 0x18, 0x09,
    0x3e, 0x81, 0xd2, 0xa6, 0x3c, 0x32, 0xef, 0xf1, 0xd9, 0x03, 0x0f, 0x9e, 0x14, 0x14, 0xec, 0xe4,
    0x20, 0xda, 0xa2, 0x4e, 0x0d, 0xd5, 0xb8, 0x45, 0xb3, 0x27, 0x4b, 0xb8, 0x39, 0xca, 0x1c, 0x53,
    0xbc, 0xc0, 0x19, 0x42, 0x42, 0xd7, 0x4b, 0x26, 0x31, 0xb9, 0x49, 0x5a, 0x65, 0x4f, 0xbb, 0xdc,
    0xbf, 0xad, 0x77, 0x9f, 0x73, 0x22, 0xb6, 0x07, 0x36, 0x24, 0x98, 0x80, 0x60, 0x48, 0x21, 0xd9,
    0x69, 0x24, 0xe3, 0xfa, 0x39, 0x7f, 0x35, 0x4a, 0x5e, 0xcc, 0xa3, 0x4f, 0x61, 0x4d, 0xa5, 0x45,
    0x6f, 0x9b, 0x36, 0x33, 0x8c, 0x37, 0xd8, 0xf6, 0xfb, 0xf6, 0x26, 0xbe, 0x98, 0x34, 0x77, 0x76,
    0x60, 0x22, 0x87, 0x27, 0x46, 0xda, 0x10, 0xa1, 0x77, 0x1c, 0xeb, 0x02, 0xdd, 0x8a, 0xac, 0x01,
    0xba, 0x18, 0x6b, 0xf1, 0x48, 0x86, 0x30, 0x47, 0x9e, 0x12, 0x84, 0xda, 0x01, 0x90, 0xfc, 0xe8,
    0xb5, 0x9a, 0xc6, 0xb0, 0xfd, 0x41, 0x6b, 0xee, 0x56, 0xb7, 0x2f, 0x0a, 0x58, 0x45, 0x15, 0x35,
    0x57, 0xff, 0x0f, 0x49, 0x50, 0xa0, 0xdc, 0x5b, 0xe6, 0x5c, 0xe9, 0x42, 0xd2, 0x2e, 0x18, 0x53,
    0x4c, 0x4e, 0x0e, 0xfa, 0xbb, 0x2d, 0x15, 0x25, 0xdc, 0x48, 0x58, 0xb9, 0xb0, 0xf7, 0x7d, 0x47,
    0x4a, 0x12, 0x5e, 0xbc, 0x25, 0x0e, 0x08, 0xfe, 0xdb, 0xfa, 0xa6, 0x6f, 0x45, 0x3d, 0x90, 0x93,
    0x2c, 0xab, 0x3f, 0xf4, 0x52, 0x21, 0x90, 0x99, 0x68, 0xe5, 0x1e, 0x6b, 0xc2, 0x54, 0xd5, 0x09,
    0xad, 0xeb, 0x75, 0xcb, 0xa7, 0x6d, 0x48, 0xfe, 0x02, 0x4e, 0x3e, 0x66, 0xd8, 0xdf, 0x5e,
];

struct BlockTestVector(pub [u8; 3280]);

impl fmt::Debug for BlockTestVector {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("BlockTestVector")
            .field(&hex::encode(&self.0[..]))
            .finish()
    }
}

lazy_static! {
    pub static ref BLOCK_MAINNET_415000_BYTES: Vec<u8> = <Vec<u8>>::from_hex("040000005274b43b9e4ad8f43e93f78463d24dcfe531aeb4719819f4f97f7e0300000000663073bc4bfa95c9bec36aad7268a573049797bdfc5aa4c743fbe4820aa393ce0000000000000000000000000000000000000000000000000000000000000000a8becc5be1ab031cc2fd607c776a7a0000000000000000000000000000000000000000003eb21819fd400500949d55de0cc633e0cce41e4649ef4aa3349f0100290ffe281b947b3b53fbd2f35b1ce292649b96ac6e0883af3a6844b95592e74556da344b4701961cd4130c68219cfa1341d5afb5049eb0e8be4a2d92d678c40785e33705548b5f3a54f0a4c39a2f58ee784a24163cd86f54812327df55e1d55ca84b6e7b887a7cbfb9091a585bdb8ea4759307c56c1b3dafc669245a6f654b6f730052266a01ad4f9c0b59ed4e17712b3e72df0498aa8de4888f993531c60acded1d4b66e89de0b6482cccd4a712f5cf9d4ca83be0f922de2c1dbb3a1407480dbe8795993d8be640988abfe7a8a1b33a12131c451e1abc0d83fb851862c637ce724d5fe97aa9a806cf34bab509f4554b0cd10a7ddfd5821b091ad2c90c1aa1d81eb3d72db41993b648f41e2138ff9531a30ff73b22140e4ebd7baa33848e512d99300c5c131c6e75f5714a5c6dcb178b4a4978dac83ad412fbd692019250c553049aad457984bedfc96ae701c659bc7007a97d0a9002b945bdec45a945ef6285b2cd553b4c09d907c627863f0399e8725b4ff7fc5979e3cff22814508448ef8b9831c285959333396aa362a51cf205097afabec15e41fb6e30b622374bf58b37ef9d1b241ead5a682b98b65749a57568e238d50afd417e1e960e7b5a064fd9f694d783a2cbcd58552dedbb9e5e1123674ef73a524196cf05d3e524660549ffe7bd6568057135ffd5afd943f6da11cbb597e8ccecd77ecbe909de0631bfa29cd3e3d5544671ba80256153d6e9990b88ad8e0cf4989bef4be457f9c7b0f1aacd6e0ef320605c29ed0cd2eb6cfce216c52a317580201cad7a0943d24b7b06d5bf758761dd96e11970b5ded697222b2c77e7f256a605ac755549c1651f25adfc9d53d9117e3a0bb409eee4a600120472949c7dda1c2edb3c330c7f96179982916457d331e96309dd24df74eedd00e7db497ee130f77de666eb557fb316e87adaf1813ce426a458a6eee3a85b2ab88f6553aadae8de652e211a1d9f334d596b5eb6173407efcc2e8154bb9ca1212aa9a1a1121d2f5a7712cf25cc8148b8052e0d2e09f20e5ba2a98277e975b0eed9a89206966337163f215c9d04a6598b0958d333d846773c69e5abfd0a0427f3660614dd82b79adb851a0d58b62df5f0b3ac836e6e25f3a51f49a99ade57796fe9fcc26f0a1f94ff0819fe52b75087edbed3a81626eb5416c66557f11c0fcedff223d6aa8cd5c35386e5b4b95a0f0392ca301a38b3687d094493b9e9d264d07a190ce57d116804382a3fabe15af4df4fa043f0287aa1ed5568d9ef5d12510d010ccdab4eb616f6df13bb3126ef43d9d65735e4e4c04b576348d040b535055a3d5ae191b75f0612f3b24066a05245f27fe57bda66bd6dec7e4fc9cb236802062adde3cd0e313482c92a0c721102b1f38b015ab8d01559cbcb40f674e9efad5ee9c2fe133faa55ca1dd0ff26710f9da819cc1459cb7ed260dad3db0596258d47c74c32a8b852b671c5a0caa2001603d90c91a7df2e2d4ee9ae9bf1a6b1ec88151c62360d03024d2e2d0114084f6b88c5bba24aa7cecfac16e91e0baf3d8653e218093e81d2a63c32eff1d9030f9e1414ece420daa24e0dd5b845b3274bb839ca1c53bcc0194242d74b2631b9495a654fbbdcbfad779f7322b60736249880604821d96924e3fa397f354a5ecca34f614da5456f9b36338c37d8f6fbf626be983477766022872746da10a1771ceb02dd8aac01ba186bf1488630479e1284da0190fce8b59ac6b0fd416bee56b72f0a5845153557ff0f4950a0dc5be65ce942d22e18534c4e0efabb2d1525dc4858b9b0f77d474a125ebc250e08fedbfaa66f453d90932cab3ff45221909968e51e6bc254d509adeb75cba76d48fe024e3e66d8df5e01030000807082c403010000000000000000000000000000000000000000000000000000000000000000ffffffff1a03185506152f5669614254432f48656c6c6f20776f726c64212fffffffff0200ca9a3b000000001976a914fb8a6a4c11cb216ce21f9f371dfc9271a469bd6d88ac80b2e60e0000000017a914e0a5ea1340cc6b1d6a82c06c0a9c60b9898b6ae987000000000000000000").expect("Block bytes are in valid hex representation");
}

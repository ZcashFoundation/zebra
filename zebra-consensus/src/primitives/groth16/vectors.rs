use crate::groth16::h_sig;

#[test]
fn h_sig_works() {
    // Test vector from zcash:
    // https://github.com/zcash/zcash/blob/2c17d1e2740115c9c88046db4a3bb0aa069dae4f/src/gtest/test_joinsplit.cpp#L252
    let tests: [[&str; 5]; 4] = [
        [
            "6161616161616161616161616161616161616161616161616161616161616161",
            "6262626262626262626262626262626262626262626262626262626262626262",
            "6363636363636363636363636363636363636363636363636363636363636363",
            "6464646464646464646464646464646464646464646464646464646464646464",
            "a8cba69f1fa329c055756b4af900f8a00b61e44f4cb8a1824ceb58b90a5b8113",
        ],
        [
            "0000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "697322276b5dd93b12fb1fcbd2144b2960f24c73aac6c6a0811447be1e7f1e19",
        ],
        [
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "4961048919f0ca79d49c9378c36a91a8767060001f4212fe6f7d426f3ccf9f32",
        ],
        [
            "1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020100",
            "1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020100",
            "1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020100",
            "1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020100",
            "b61110ec162693bc3d9ca7fb0eec3afd2e278e2f41394b3ff11d7cb761ad4b27",
        ],
    ];

    for t in tests {
        // Test vectors are byte-reversed (because they are loaded in zcash
        // with a function that loads in reverse order), so we need to re-reverse them.
        let mut random_seed = hex::decode(t[0]).unwrap();
        random_seed.reverse();
        let mut nf1 = hex::decode(t[1]).unwrap();
        nf1.reverse();
        let mut nf2 = hex::decode(t[2]).unwrap();
        nf2.reverse();
        let mut pubkey = hex::decode(t[3]).unwrap();
        pubkey.reverse();
        let mut r = h_sig(
            &<[u8; 32]>::try_from(random_seed).unwrap().into(),
            &<[u8; 32]>::try_from(nf1).unwrap().into(),
            &<[u8; 32]>::try_from(nf2).unwrap().into(),
            &<[u8; 32]>::try_from(pubkey).unwrap().into(),
        );
        r.reverse();

        assert_eq!(hex::encode(r), t[4]);
    }
}

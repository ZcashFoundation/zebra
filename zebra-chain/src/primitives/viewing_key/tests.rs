//! Tests for Zebra history trees

use color_eyre::eyre::eyre;

use super::*;

#[test]
fn key_hash_can_be_reproduced() -> color_eyre::Result<()> {
    let _init_guard = zebra_test::init();

    /// The extended Sapling viewing key of [ZECpages](https://zecpages.com/boardinfo)
    pub const ZECPAGES_SAPLING_VIEWING_KEY: &str = "zxviews1q0duytgcqqqqpqre26wkl45gvwwwd706xw608hucmvfalr759ejwf7qshjf5r9aa7323zulvz6plhttp5mltqcgs9t039cx2d09mgq05ts63n8u35hyv6h9nc9ctqqtue2u7cer2mqegunuulq2luhq3ywjcz35yyljewa4mgkgjzyfwh6fr6jd0dzd44ghk0nxdv2hnv4j5nxfwv24rwdmgllhe0p8568sgqt9ckt02v2kxf5ahtql6s0ltjpkckw8gtymxtxuu9gcr0swvz";

    let viewing_key = ViewingKey::parse(ZECPAGES_SAPLING_VIEWING_KEY, Network::Mainnet)
        .map_err(|err| eyre!(err))?;

    assert_eq!(
        KeyHash::new(&viewing_key),
        KeyHash::new(&viewing_key),
        "key hashes for a given viewing key should match",
    );

    Ok(())
}

//! Tests for zebra-chain viewing key hashes

use super::*;

/// The extended Sapling viewing key of [ZECpages](https://zecpages.com/boardinfo)
pub const ZECPAGES_SAPLING_VIEWING_KEY: &str = "zxviews1q0duytgcqqqqpqre26wkl45gvwwwd706xw608hucmvfalr759ejwf7qshjf5r9aa7323zulvz6plhttp5mltqcgs9t039cx2d09mgq05ts63n8u35hyv6h9nc9ctqqtue2u7cer2mqegunuulq2luhq3ywjcz35yyljewa4mgkgjzyfwh6fr6jd0dzd44ghk0nxdv2hnv4j5nxfwv24rwdmgllhe0p8568sgqt9ckt02v2kxf5ahtql6s0ltjpkckw8gtymxtxuu9gcr0swvz";

/// Tests that `ViewingKey::parse` successfully decodes the zecpages sapling extended full viewing key
#[test]
fn parses_sapling_efvk_correctly() {
    let _init_guard = zebra_test::init();

    ViewingKey::parse(ZECPAGES_SAPLING_VIEWING_KEY, Network::Mainnet)
        .expect("should parse hard-coded viewing key successfully");
}

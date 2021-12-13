use super::super::serialize::parse_coinbase_height;

#[test]
fn parse_coinbase_height_mins() {
    zebra_test::init();

    // examples with height 1:

    let case1 = vec![0x51];
    assert!(parse_coinbase_height(case1.clone()).is_ok());
    assert_eq!(parse_coinbase_height(case1).unwrap().0 .0, 1);

    let case2 = vec![0x01, 0x01];
    assert!(parse_coinbase_height(case2).is_err());

    let case3 = vec![0x02, 0x01, 0x00];
    assert!(parse_coinbase_height(case3).is_err());

    let case4 = vec![0x03, 0x01, 0x00, 0x00];
    assert!(parse_coinbase_height(case4).is_err());

    let case5 = vec![0x04, 0x01, 0x00, 0x00, 0x00];
    assert!(parse_coinbase_height(case5).is_err());

    // examples with height 17:

    let case1 = vec![0x01, 0x11];
    assert!(parse_coinbase_height(case1.clone()).is_ok());
    assert_eq!(parse_coinbase_height(case1).unwrap().0 .0, 17);

    let case2 = vec![0x02, 0x11, 0x00];
    assert!(parse_coinbase_height(case2).is_err());

    let case3 = vec![0x03, 0x11, 0x00, 0x00];
    assert!(parse_coinbase_height(case3).is_err());

    let case4 = vec![0x04, 0x11, 0x00, 0x00, 0x00];
    assert!(parse_coinbase_height(case4).is_err());
}

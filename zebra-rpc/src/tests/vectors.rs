use crate::methods::GetRawTransaction;

#[test]
pub fn test_transaction_serialization() {
    let expected_tx = GetRawTransaction::Raw(vec![0x42].into());
    let expected_json = r#""42""#;
    let j = serde_json::to_string(&expected_tx).unwrap();

    assert_eq!(j, expected_json);

    let expected_tx = GetRawTransaction::Object {
        hex: vec![0x42].into(),
        height: 1,
    };
    let expected_json = r#"{"hex":"42","height":1}"#;
    let j = serde_json::to_string(&expected_tx).unwrap();

    assert_eq!(j, expected_json);
}

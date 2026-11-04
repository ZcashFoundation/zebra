//! Differential compatibility tests and an opt-in large-payload microbenchmark.

use super::{decode, encode, HexData};

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ReferenceHex(#[serde(with = "hex")] Vec<u8>);

#[test]
fn hex_conversion_matches_reference() {
    let bytes: Vec<_> = (0..=255).collect();
    for len in 0..=bytes.len() {
        let bytes = &bytes[..len];
        let encoded = encode(bytes);
        assert_eq!(encoded, hex::encode(bytes));
        assert_eq!(decode(encoded.as_bytes()), hex::decode(&encoded));
        assert_eq!(decode(encoded.to_uppercase().as_bytes()).unwrap(), bytes);
    }

    for first in 0..=255 {
        for second in 0..=255 {
            let pair = [first, second];
            assert_eq!(decode(&pair), hex::decode(pair));
        }
    }

    // Check every possible input byte at every position, including error indices.
    let mut input = b"0123456789aBcDeF0123456789AbCdEf01".to_vec();
    for index in 0..input.len() {
        let original = input[index];
        for byte in 0..=255 {
            input[index] = byte;
            assert_eq!(decode(&input), hex::decode(&input));
        }
        input[index] = original;
    }

    for input in [
        "", "0", "fF0", "g", "0x00", "0X00", " f", "f\n", "é", "0é0", "💻",
    ] {
        assert_eq!(decode(input.as_bytes()), hex::decode(input));
    }
}

#[test]
fn hex_serde_matches_reference() {
    for json in [
        r#""""#,
        r#""00aBcDeF""#,
        r#""0x00""#,
        r#""0X00""#,
        r#""0""#,
        r#""g""#,
        r#""0g""#,
        r#""g0""#,
        r#""é""#,
        r#""0é0""#,
        r#""\u0030\u0061""#,
        r#""00\n""#,
        r#"null"#,
        r#"true"#,
        r#"42"#,
        r#"[]"#,
        r#"[0, 255]"#,
        r#"{}"#,
    ] {
        let actual = serde_json::from_str::<HexData>(json)
            .map(|data| data.0)
            .map_err(|error| error.to_string());
        let expected = serde_json::from_str::<ReferenceHex>(json)
            .map(|data| data.0)
            .map_err(|error| error.to_string());
        assert_eq!(actual, expected, "input: {json}");
    }

    for bytes in [Vec::new(), (0..=255).collect()] {
        let expected = serde_json::to_string(&ReferenceHex(bytes.clone())).unwrap();
        let actual = serde_json::to_string(&HexData(bytes.clone())).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(serde_json::from_str::<HexData>(&actual).unwrap().0, bytes);
    }
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct OptionalHex {
    #[serde(
        default,
        with = "super::option",
        skip_serializing_if = "Option::is_none"
    )]
    data: Option<Vec<u8>>,
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct OptionalHexWithNull {
    #[serde(with = "super::option")]
    data: Option<Vec<u8>>,
}

#[test]
fn optional_hex_serde() {
    for (json, expected) in [
        (r#"{}"#, None),
        (r#"{"data":null}"#, None),
        (r#"{"data":""}"#, Some(Vec::new())),
        (r#"{"data":"00aBcDeF"}"#, Some(vec![0x00, 0xab, 0xcd, 0xef])),
    ] {
        assert_eq!(
            serde_json::from_str::<OptionalHex>(json).unwrap(),
            OptionalHex { data: expected },
            "input: {json}"
        );
    }

    // Invalid strings fail with the same text as the non-optional form.
    for data in [r#""0""#, r#""0g""#, r#""0x00""#] {
        let optional = serde_json::from_str::<OptionalHex>(&format!(r#"{{"data":{data}}}"#))
            .unwrap_err()
            .to_string();
        let required = serde_json::from_str::<HexData>(data)
            .unwrap_err()
            .to_string();
        let required = required
            .split(" at line")
            .next()
            .expect("split returns at least one part");
        assert!(optional.starts_with(required), "{optional} / {required}");
    }
    for data in ["true", "42", "[]", "{}"] {
        assert!(
            serde_json::from_str::<OptionalHex>(&format!(r#"{{"data":{data}}}"#)).is_err(),
            "input: {data}"
        );
    }

    let bytes: Vec<u8> = (0..=255).collect();
    let json = serde_json::to_string(&OptionalHex {
        data: Some(bytes.clone()),
    })
    .unwrap();
    assert_eq!(json, format!(r#"{{"data":"{}"}}"#, hex::encode(&bytes)));
    assert_eq!(
        serde_json::from_str::<OptionalHex>(&json).unwrap().data,
        Some(bytes)
    );
    assert_eq!(
        serde_json::to_string(&OptionalHex { data: None }).unwrap(),
        "{}"
    );
    assert_eq!(
        serde_json::to_string(&OptionalHexWithNull { data: None }).unwrap(),
        r#"{"data":null}"#
    );
}

/// Run with `cargo test -p zebra-rpc --release benchmark_large_hex -- --ignored --nocapture`.
#[test]
#[ignore = "release-mode microbenchmark, not a timing assertion"]
#[expect(
    clippy::print_stdout,
    reason = "this opt-in microbenchmark reports its measurements"
)]
fn benchmark_large_hex() {
    use std::{hint::black_box, time::Instant};

    fn measure<T>(mut convert: impl FnMut() -> T) -> f64 {
        let start = Instant::now();
        for _ in 0..8 {
            black_box(convert());
        }
        start.elapsed().as_secs_f64() * 1000.0 / 8.0
    }

    fn compare<T>(label: &str, mut old: impl FnMut() -> T, mut new: impl FnMut() -> T) {
        // Warm both implementations, then alternate the order to reduce drift bias.
        black_box(old());
        black_box(new());
        let mut old_ms = [0.0; 9];
        let mut new_ms = [0.0; 9];
        let mut speedups = [0.0; 9];
        for index in 0..old_ms.len() {
            if index % 2 == 0 {
                old_ms[index] = measure(&mut old);
                new_ms[index] = measure(&mut new);
            } else {
                new_ms[index] = measure(&mut new);
                old_ms[index] = measure(&mut old);
            }
            speedups[index] = old_ms[index] / new_ms[index];
        }
        old_ms.sort_by(f64::total_cmp);
        new_ms.sort_by(f64::total_cmp);
        speedups.sort_by(f64::total_cmp);
        println!(
            "{label}: old {:.3} ms [{:.3}, {:.3}], new {:.3} ms [{:.3}, {:.3}], \
             speedup {:.2}x [{:.2}, {:.2}] (median [min, max], 9 interleaved samples of 8)",
            old_ms[4],
            old_ms[0],
            old_ms[8],
            new_ms[4],
            new_ms[0],
            new_ms[8],
            speedups[4],
            speedups[0],
            speedups[8],
        );
    }

    for size in [1_900_000, 3_800_000] {
        let bytes: Vec<_> = (0..=255).cycle().take(size).collect();
        let encoded = hex::encode(&bytes);
        assert_eq!(encode(&bytes), encoded);
        assert_eq!(decode(encoded.as_bytes()).unwrap(), bytes);
        println!("{size} binary bytes / {} hex bytes", encoded.len());
        compare(
            "decode",
            || hex::decode(black_box(encoded.as_bytes())).unwrap(),
            || decode(black_box(encoded.as_bytes())).unwrap(),
        );
        compare(
            "encode",
            || hex::encode(black_box(&bytes)),
            || encode(black_box(&bytes)),
        );
    }
}

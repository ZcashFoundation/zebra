//! Tests for ZEC amount parsing.

use proptest::prelude::*;

use zebra_chain::amount::{Amount, NonNegative, MAX_MONEY};

use super::Zec;

/// Parses `json` as a [`Zec`] and returns the zatoshis.
fn parse(json: &str) -> i64 {
    serde_json::from_str::<Zec<NonNegative>>(json)
        .unwrap_or_else(|err| panic!("{json} should parse: {err}"))
        .zatoshis()
}

/// Values Zebra emits whose product with `COIN` is not an integer in `f64`.
#[test]
fn parses_amounts_with_inexact_float_products() {
    assert_eq!(parse("0.00000003"), 3);
    assert_eq!(parse("8.00000001"), 800_000_001);
    assert_eq!(parse("529544.04149098"), 52_954_404_149_098);
    assert_eq!(parse("8599562.98397532"), 859_956_298_397_532);

    let max = serde_json::to_string(&Zec::from(
        Amount::<NonNegative>::try_from(MAX_MONEY).unwrap(),
    ))
    .unwrap();
    assert_eq!(parse(&max), MAX_MONEY);
}

/// Fractions of a zatoshi are rounded, not rejected, because serialized amounts carry up to a
/// third of a zatoshi of floating point error.
#[test]
fn rounds_fractions_of_a_zatoshi() {
    assert_eq!(parse("0.000000004"), 0);
    assert_eq!(parse("-0.000000004"), 0);
    assert_eq!(parse("0.000000006"), 1);
}

#[test]
fn rejects_non_finite_and_out_of_range_values() {
    assert!(Zec::<NonNegative>::from_lossy_zec(f64::NAN).is_err());
    assert!(Zec::<NonNegative>::from_lossy_zec(f64::INFINITY).is_err());
    assert!(Zec::<NonNegative>::from_lossy_zec(-0.00000001).is_err());
    assert!(serde_json::from_str::<Zec<NonNegative>>("21000000.00000001").is_err());
}

proptest! {
    /// Every amount survives a JSON round trip through the lossy `f64` representation.
    #[test]
    fn json_round_trip(zats in 0..=MAX_MONEY) {
        let amount = Amount::<NonNegative>::try_from(zats).unwrap();
        let json = serde_json::to_string(&Zec::from(amount)).unwrap();
        let parsed: Zec<NonNegative> = serde_json::from_str(&json).unwrap();

        prop_assert_eq!(parsed.zatoshis(), zats, "{}", json);
    }
}

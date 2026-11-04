//! Tests for the second argument of the `getblock` RPC.

// `insta`'s macro expansion trips this lint, and each toolchain knows only one of its two names: https://github.com/rust-lang/rust/issues/79813
#![allow(unknown_lints)]
#![allow(semicolon_in_expressions_from_macros)]
#![allow(semicolon_in_expressions_from_non_local_macros)]

use std::collections::BTreeSet;

use jsonrpsee_types::{ErrorObjectOwned, Params};
use proptest::prelude::*;
use serde_json::{json, Value};

use super::{GetBlockField as F, GetBlockQuery};

/// Every field and its JSON name, in declaration order, which is also the `Ord` order.
const FIELDS: [(F, &str); 25] = [
    (F::Hash, "hash"),
    (F::Confirmations, "confirmations"),
    (F::Size, "size"),
    (F::Height, "height"),
    (F::Version, "version"),
    (F::MerkleRoot, "merkleroot"),
    (F::BlockCommitments, "blockcommitments"),
    (F::AuthDataRoot, "authdataroot"),
    (F::FinalSaplingRoot, "finalsaplingroot"),
    (F::FinalOrchardRoot, "finalorchardroot"),
    (F::TxCount, "nTx"),
    (F::Tx, "tx"),
    (F::TxDetails, "txdetails"),
    (F::TxPrevouts, "txprevouts"),
    (F::Time, "time"),
    (F::Nonce, "nonce"),
    (F::Solution, "solution"),
    (F::Bits, "bits"),
    (F::Difficulty, "difficulty"),
    (F::ChainSupply, "chainSupply"),
    (F::ValuePools, "valuePools"),
    (F::Trees, "trees"),
    (F::PreviousBlockHash, "previousblockhash"),
    (F::NextBlockHash, "nextblockhash"),
    (F::Hex, "hex"),
];

/// The visitor's description of the accepted argument shapes.
const EXPECTING: &str =
    "a verbosity level from 0 to 3, a boolean, or an array of getblock field names";

/// The visitor's description of an accepted field list element.
const EXPECTING_FIELD: &str = "a getblock field name";

/// Returns the field `name` selects, including through the `txids` alias.
fn field(name: &str) -> Option<F> {
    let name = if name == "txids" { "tx" } else { name };
    FIELDS
        .iter()
        .find(|(_, field_name)| *field_name == name)
        .map(|(field, _)| *field)
}

/// Returns the fields in `fields` as a field query.
fn fields<const N: usize>(fields: [F; N]) -> GetBlockQuery {
    GetBlockQuery::Fields(BTreeSet::from(fields))
}

/// Parses `json` the way jsonrpsee parses a positional parameter.
fn parse(json: &str) -> Result<GetBlockQuery, serde_json::Error> {
    serde_json::from_str(json)
}

/// Returns the error text for `json`, which must not parse.
fn parse_error(json: &str) -> String {
    match parse(json) {
        Ok(query) => panic!("{json} must not parse, but parsed as {query:?}"),
        Err(error) => error.to_string(),
    }
}

/// Returns the second positional parameter of `params` as the generated server code reads it.
fn second_param(params: &str) -> Result<Option<GetBlockQuery>, ErrorObjectOwned> {
    let params = Params::new(Some(params));
    let mut sequence = params.sequence();
    let _hash_or_height: String = sequence.next()?;
    sequence.optional_next()
}

/// The start of serde_json's description of an unexpected JSON value of this type.
fn unexpected(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) if number.is_f64() => "floating point",
        Value::Number(_) => "integer",
        Value::String(_) => "string",
        Value::Array(_) => "sequence",
        Value::Object(_) => "map",
    }
}

/// How the error for a rejected value must read.
#[derive(Debug)]
struct Rejection {
    /// The start of the error text, which describes the rejected value or element.
    start: String,
    /// What the error text says was expected, after the description of the rejected value.
    expecting: &'static str,
}

impl Rejection {
    fn new(start: String, expecting: &'static str) -> Self {
        Self { start, expecting }
    }

    fn matches(&self, error: &str) -> bool {
        error.starts_with(&self.start) && error.contains(&format!(", expected {}", self.expecting))
    }
}

/// Returns the query `value` must parse as, or how its rejection must read.
///
/// An array is rejected at its first element that is not a field name, as serde reads in order.
fn expected(value: &Value) -> Result<GetBlockQuery, Rejection> {
    match value {
        Value::Bool(verbose) => Ok(GetBlockQuery::Verbosity(u8::from(*verbose))),
        Value::Number(number) if !number.is_f64() => number
            .as_u64()
            .and_then(|verbosity| u8::try_from(verbosity).ok())
            .map(GetBlockQuery::Verbosity)
            .ok_or_else(|| Rejection::new(format!("invalid value: integer `{number}`"), EXPECTING)),
        Value::Array(elements) => {
            let mut query = BTreeSet::new();
            for element in elements {
                let rejection = match element {
                    Value::String(name) => match field(name) {
                        Some(field) => {
                            query.insert(field);
                            continue;
                        }
                        None => Rejection::new(
                            format!("unknown variant `{name}`"),
                            "one of `hash`, `confirmations`, ",
                        ),
                    },
                    other => Rejection::new(
                        format!("invalid type: {}", unexpected(other)),
                        EXPECTING_FIELD,
                    ),
                };
                return Err(rejection);
            }
            Ok(GetBlockQuery::Fields(query))
        }
        other => Err(Rejection::new(
            format!("invalid type: {}", unexpected(other)),
            EXPECTING,
        )),
    }
}

#[test]
fn field_names_are_the_response_keys() {
    let _init_guard = zebra_test::init();

    for (field, name) in FIELDS {
        assert_eq!(serde_json::to_value(field).unwrap(), json!(name));
        assert_eq!(serde_json::from_value::<F>(json!(name)).unwrap(), field);
    }
    assert!(
        FIELDS.windows(2).all(|pair| pair[0].0 < pair[1].0),
        "the declaration order is the `Ord` order, so sets serialize in the response key order"
    );
    assert_eq!(serde_json::from_value::<F>(json!("txids")).unwrap(), F::Tx);

    // serde lists every accepted name, aliases included, so a new field cannot go unlisted.
    let accepted = FIELDS
        .iter()
        .flat_map(|(_, name)| {
            if *name == "tx" {
                vec!["tx", "txids"]
            } else {
                vec![*name]
            }
        })
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let error = parse_error(r#"["merkelroot"]"#);
    assert!(
        error.starts_with(&format!(
            "unknown variant `merkelroot`, expected one of {accepted} at line 1 column"
        )),
        "{error}"
    );
}

#[test]
fn integers_and_booleans_are_verbosity_levels() {
    let _init_guard = zebra_test::init();

    for verbosity in 0..=u8::MAX {
        assert_eq!(
            parse(&verbosity.to_string()).unwrap(),
            GetBlockQuery::Verbosity(verbosity)
        );
        assert_eq!(
            serde_json::to_string(&GetBlockQuery::Verbosity(verbosity)).unwrap(),
            verbosity.to_string()
        );
    }
    assert_eq!(parse("true").unwrap(), GetBlockQuery::Verbosity(1));
    assert_eq!(parse("false").unwrap(), GetBlockQuery::Verbosity(0));
}

#[test]
fn field_lists_are_sets() {
    let _init_guard = zebra_test::init();

    let query = parse(r#"["trees", "tx", "hash", "tx", "txids"]"#).unwrap();
    assert_eq!(query, fields([F::Hash, F::Tx, F::Trees]));
    assert_eq!(
        serde_json::to_string(&query).unwrap(),
        r#"["hash","tx","trees"]"#
    );

    // The handler rejects these with `-8`, so they must reach it.
    assert_eq!(parse("[]").unwrap(), fields([]));
    assert_eq!(
        parse(r#"["txprevouts", "txdetails", "tx"]"#).unwrap(),
        fields([F::Tx, F::TxDetails, F::TxPrevouts])
    );

    // Escaped names are names like any other.
    let escaped = concat!(r#"[""#, "\\u0068", r#"ash"]"#);
    assert_eq!(parse(escaped).unwrap(), fields([F::Hash]));
}

#[test]
fn rejected_arguments_keep_their_cause() {
    let _init_guard = zebra_test::init();

    for (json, cause) in [
        ("256", "invalid value: integer `256`"),
        ("-1", "invalid value: integer `-1`"),
        ("1.5", "invalid type: floating point `1.5`"),
        (r#""1""#, r#"invalid type: string "1""#),
        ("{}", "invalid type: map"),
        (r#"{"fields": ["hash"]}"#, "invalid type: map"),
        ("null", "invalid type: null"),
    ] {
        let error = parse_error(json);
        assert!(
            error.starts_with(&format!("{cause}, expected {EXPECTING} at line 1 column")),
            "{json}: {error}"
        );
    }

    for (json, cause) in [
        (
            r#"["Hash"]"#,
            "unknown variant `Hash`, expected one of `hash`, ",
        ),
        (
            r#"["hash", "chainhistoryroot"]"#,
            "unknown variant `chainhistoryroot`, ",
        ),
        (
            "[1]",
            "invalid type: integer `1`, expected a getblock field name",
        ),
        (
            r#"["hash", true]"#,
            "invalid type: boolean `true`, expected a getblock field name",
        ),
        (
            "[null]",
            "invalid type: null, expected a getblock field name",
        ),
        (
            r#"[["hash"]]"#,
            "invalid type: sequence, expected a getblock field name",
        ),
        (
            r#"[{"hash": null}]"#,
            "invalid type: map, expected a getblock field name",
        ),
    ] {
        let error = parse_error(json);
        assert!(error.starts_with(cause), "{json}: {error}");
    }
}

#[test]
fn second_positional_parameter() {
    let _init_guard = zebra_test::init();

    assert_eq!(second_param(r#"["1"]"#).unwrap(), None);
    assert_eq!(second_param(r#"["1", null]"#).unwrap(), None);
    assert_eq!(
        second_param(r#"["1", 2]"#).unwrap(),
        Some(GetBlockQuery::Verbosity(2))
    );
    assert_eq!(
        second_param(r#"["1", true]"#).unwrap(),
        Some(GetBlockQuery::Verbosity(1))
    );
    assert_eq!(
        second_param(r#"["1", ["hash", "tx", "trees"]]"#).unwrap(),
        Some(fields([F::Hash, F::Tx, F::Trees]))
    );

    for (params, cause) in [
        (
            r#"["1", [1]]"#,
            "invalid type: integer `1`, expected a getblock field name",
        ),
        (
            r#"["1", ["merkelroot"]]"#,
            "unknown variant `merkelroot`, expected one of ",
        ),
        (
            r#"["1", 256]"#,
            "invalid value: integer `256`, expected a verbosity level",
        ),
    ] {
        let error = second_param(params).unwrap_err();
        assert_eq!(error.code(), -32602, "{params}");
        assert_eq!(error.message(), "Invalid params", "{params}");
        let data: String = serde_json::from_str(
            error
                .data()
                .expect("jsonrpsee puts the deserialization error in the data")
                .get(),
        )
        .unwrap();
        assert!(data.starts_with(cause), "{params}: {data}");
    }
}

#[test]
fn schema() {
    let _init_guard = zebra_test::init();

    let schema = schemars::schema_for!(GetBlockQuery);
    assert_eq!(
        schema.get("anyOf").and_then(|shapes| shapes.get(0)),
        Some(&json!({"type": "integer", "format": "uint8", "minimum": 0, "maximum": 3})),
        "the published verbosity range must be the one the handler accepts"
    );
    // zebrad's dev-dependencies enable `serde_json/preserve_order`, so sort the maps to keep the
    // snapshot independent of the packages a test run selects.
    insta::with_settings!({ sort_maps => true }, {
        insta::assert_json_snapshot!("get_block_query_schema", schema);
    });
}

/// Any accepted field name, including the `txids` alias.
fn field_name() -> impl Strategy<Value = &'static str> {
    prop::sample::select(
        FIELDS
            .iter()
            .map(|(_, name)| *name)
            .chain(["txids"])
            .collect::<Vec<_>>(),
    )
}

/// Any JSON value, with field names, field lists, and integers near the `u8` range made likely.
fn json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        any::<u64>().prop_map(Value::from),
        (0_u64..300).prop_map(Value::from),
        any::<f64>().prop_filter_map("JSON numbers are finite", |number| {
            serde_json::Number::from_f64(number).map(Value::Number)
        }),
        ".*".prop_map(Value::from),
        field_name().prop_map(Value::from),
    ];

    leaf.prop_recursive(3, 32, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::vec(field_name().prop_map(Value::from), 0..8).prop_map(Value::Array),
            prop::collection::btree_map(".*", inner, 0..4)
                .prop_map(|map| Value::Object(map.into_iter().collect())),
        ]
    })
}

proptest! {
    /// Verbosity levels round trip as JSON integers.
    #[test]
    fn verbosity_round_trip(verbosity in any::<u8>()) {
        let query = GetBlockQuery::Verbosity(verbosity);
        let json = serde_json::to_string(&query).unwrap();

        prop_assert_eq!(&json, &verbosity.to_string());
        prop_assert_eq!(parse(&json).unwrap(), query);
    }

    /// Field lists parse as sets, in any order and with duplicates, and serialize as the sorted set.
    #[test]
    fn field_lists_round_trip(names in prop::collection::vec(field_name(), 0..40)) {
        let query = parse(&serde_json::to_string(&names).unwrap()).unwrap();
        let set: BTreeSet<F> = names.iter().map(|name| field(name).unwrap()).collect();
        prop_assert_eq!(&query, &GetBlockQuery::Fields(set.clone()));

        let json = serde_json::to_string(&query).unwrap();
        prop_assert_eq!(
            serde_json::from_str::<Vec<F>>(&json).unwrap(),
            set.into_iter().collect::<Vec<_>>()
        );
        prop_assert_eq!(parse(&json).unwrap(), query);
    }

    /// Anything but an integer from 0 to 255, a boolean, or an array of field names is rejected,
    /// with the cause in the error text, whether it arrives as text or as a parsed JSON value.
    #[test]
    fn other_values_are_rejected(value in json_value()) {
        let from_text = parse(&value.to_string()).map_err(|error| error.to_string());
        let from_value =
            serde_json::from_value::<GetBlockQuery>(value.clone()).map_err(|error| error.to_string());

        match expected(&value) {
            Ok(query) => {
                prop_assert_eq!(from_text, Ok(query.clone()));
                prop_assert_eq!(from_value, Ok(query));
            }
            Err(rejection) => {
                let from_text = from_text.expect_err("rejected values must not parse from text");
                let from_value = from_value.expect_err("rejected values must not parse from a value");
                prop_assert!(rejection.matches(&from_text), "{} {:?} {}", value, rejection, from_text);
                prop_assert!(rejection.matches(&from_value), "{} {:?} {}", value, rejection, from_value);
            }
        }
    }
}

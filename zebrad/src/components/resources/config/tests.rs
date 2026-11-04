//! Parsing and serialization of the `[resources]` config section.

use super::{Config, MemoryBudget, ParseMemoryBudgetError};

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

#[test]
fn memory_budget_config_parses_every_unit() {
    let _init_guard = zebra_test::init();

    for (budget, expected) in [
        ("auto", MemoryBudget::Auto),
        (" AUTO ", MemoryBudget::Auto),
        ("2 GiB", MemoryBudget::Bytes(2 * GIB)),
        ("2GiB", MemoryBudget::Bytes(2 * GIB)),
        ("2 gib", MemoryBudget::Bytes(2 * GIB)),
        ("1536 MiB", MemoryBudget::Bytes(1536 * MIB)),
        ("512 KiB", MemoryBudget::Bytes(512 * 1024)),
        ("2GB", MemoryBudget::Bytes(2_000_000_000)),
        ("1500 mb", MemoryBudget::Bytes(1_500_000_000)),
        ("4096 B", MemoryBudget::Bytes(4096)),
        ("0 MiB", MemoryBudget::Bytes(0)),
    ] {
        assert_eq!(budget.parse(), Ok(expected), "{budget:?}");
    }
}

#[test]
fn memory_budget_config_rejects_numbers_without_units() {
    let _init_guard = zebra_test::init();

    for (budget, expected) in [
        ("2048", ParseMemoryBudgetError::NoUnit("2048".to_string())),
        (
            "2 TiB",
            ParseMemoryBudgetError::UnknownUnit("2 TiB".to_string()),
        ),
        (
            "1.5 GiB",
            ParseMemoryBudgetError::NotANumber("1.5 GiB".to_string()),
        ),
        (
            "-1 GiB",
            ParseMemoryBudgetError::NotANumber("-1 GiB".to_string()),
        ),
        ("", ParseMemoryBudgetError::NotANumber(String::new())),
        (
            "17179869184 GiB",
            ParseMemoryBudgetError::TooLarge("17179869184 GiB".to_string()),
        ),
        (
            "18446744073709551616 B",
            ParseMemoryBudgetError::TooLarge("18446744073709551616 B".to_string()),
        ),
    ] {
        assert_eq!(budget.parse::<MemoryBudget>(), Err(expected), "{budget:?}");
    }

    assert!(
        ParseMemoryBudgetError::NoUnit("2048".to_string())
            .to_string()
            .contains("add a unit"),
        "a bare number names the fix"
    );
}

#[test]
fn memory_budget_config_displays_in_its_largest_exact_unit() {
    let _init_guard = zebra_test::init();

    for (budget, expected) in [
        (MemoryBudget::Auto, "auto"),
        (MemoryBudget::Bytes(2 * GIB), "2 GiB"),
        (MemoryBudget::Bytes(1536 * MIB), "1536 MiB"),
        (MemoryBudget::Bytes(2_000_000_000), "2 GB"),
        (MemoryBudget::Bytes(1_500_000_000), "1500 MB"),
        (MemoryBudget::Bytes(1_000_001), "1000001 B"),
    ] {
        assert_eq!(budget.to_string(), expected);
        assert_eq!(expected.parse(), Ok(budget), "{expected:?} round-trips");
    }
}

#[test]
fn resources_config_round_trips_through_toml() {
    let _init_guard = zebra_test::init();

    assert_eq!(
        toml::to_string(&Config::default()).expect("the default config serializes"),
        "",
        "an absent budget is omitted, so `zebrad generate` prints an empty section"
    );

    let config: Config =
        toml::from_str("memory_budget = \"2 GiB\"").expect("a budget with a unit parses");
    assert_eq!(config.memory_budget, Some(MemoryBudget::Bytes(2 * GIB)));
    assert_eq!(
        toml::to_string(&config).expect("a budget serializes"),
        "memory_budget = \"2 GiB\"\n"
    );

    toml::from_str::<Config>("memory_budget = 2048").expect_err("a bare number is rejected");
    toml::from_str::<Config>("memory_budget = \"2048\"").expect_err("a unit is required");
    toml::from_str::<Config>("unsafe_low_specs = true")
        .expect_err("--unsafe-low-specs is a command-line flag, not a config field");
}

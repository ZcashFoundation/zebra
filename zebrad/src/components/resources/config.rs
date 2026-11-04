//! The `[resources]` section of the zebrad config.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(test)]
mod tests;

/// Memory budget configuration.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// How much memory Zebra may use: `"auto"`, or a whole number with a unit, like `"2 GiB"` or
    /// `"1536 MiB"`.
    ///
    /// The units are `B`, `KiB`, `MiB` and `GiB` (binary), and `MB` and `GB` (decimal), in any
    /// case, with or without a space before them.
    ///
    /// Absent by default: Zebra then applies no budget, and every component keeps its usual
    /// limits.
    pub memory_budget: Option<MemoryBudget>,
}

/// A configured memory budget.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub enum MemoryBudget {
    /// Derive the budget from the memory available to this process.
    Auto,

    /// A budget of this many bytes.
    Bytes(u64),
}

/// The units a [`MemoryBudget`] accepts, with their sizes in bytes, largest first.
/// [`MemoryBudget`]'s `Display` uses the largest unit that divides the budget.
const UNITS: [(&str, u64); 6] = [
    ("GiB", 1024 * 1024 * 1024),
    ("GB", 1_000_000_000),
    ("MiB", 1024 * 1024),
    ("MB", 1_000_000),
    ("KiB", 1024),
    ("B", 1),
];

/// An error parsing a [`MemoryBudget`].
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ParseMemoryBudgetError {
    /// The budget is not `"auto"` and does not start with a whole number.
    #[error("memory_budget {0:?} must be \"auto\" or a whole number with a unit, like \"2 GiB\"")]
    NotANumber(String),

    /// The budget is a number without a unit.
    #[error("memory_budget {0:?} has no unit: add a unit, like \"{0} MiB\"")]
    NoUnit(String),

    /// The budget's unit is not one of the accepted units.
    #[error("memory_budget {0:?} has an unknown unit: use B, KiB, MiB, GiB, MB or GB")]
    UnknownUnit(String),

    /// The budget does not fit in 64 bits of bytes.
    #[error("memory_budget {0:?} is too large")]
    TooLarge(String),
}

impl FromStr for MemoryBudget {
    type Err = ParseMemoryBudgetError;

    fn from_str(budget: &str) -> Result<Self, Self::Err> {
        let trimmed = budget.trim();
        if trimmed.eq_ignore_ascii_case("auto") {
            return Ok(Self::Auto);
        }

        let digits_len = trimmed
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(trimmed.len());
        let (number, unit) = trimmed.split_at(digits_len);
        let unit = unit.trim_start();

        // A fraction like "1.5 GiB" leaves digits in the unit.
        if number.is_empty() || unit.contains(|c: char| c.is_ascii_digit()) {
            return Err(ParseMemoryBudgetError::NotANumber(budget.to_string()));
        }
        if unit.is_empty() {
            return Err(ParseMemoryBudgetError::NoUnit(budget.to_string()));
        }

        let (_, unit_bytes) = UNITS
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(unit))
            .ok_or_else(|| ParseMemoryBudgetError::UnknownUnit(budget.to_string()))?;
        let number: u64 = number
            .parse()
            .map_err(|_| ParseMemoryBudgetError::TooLarge(budget.to_string()))?;

        number
            .checked_mul(*unit_bytes)
            .map(Self::Bytes)
            .ok_or_else(|| ParseMemoryBudgetError::TooLarge(budget.to_string()))
    }
}

impl fmt::Display for MemoryBudget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => f.write_str("auto"),
            Self::Bytes(bytes) => {
                let (name, unit_bytes) = UNITS
                    .iter()
                    .find(|(_, unit_bytes)| bytes % unit_bytes == 0)
                    .expect("every byte count is a whole number of bytes, the last unit");
                write!(f, "{} {name}", bytes / unit_bytes)
            }
        }
    }
}

impl TryFrom<String> for MemoryBudget {
    type Error = ParseMemoryBudgetError;

    fn try_from(budget: String) -> Result<Self, Self::Error> {
        budget.parse()
    }
}

impl From<MemoryBudget> for String {
    fn from(budget: MemoryBudget) -> Self {
        budget.to_string()
    }
}

//! Convenience traits for converting to [`Regex`] and [`RegexSet`].

use std::iter;

use regex::{Error, Regex, RegexBuilder, RegexSet, RegexSetBuilder};

/// A trait for converting a value to a [`Regex`].
pub trait ToRegex {
    /// Converts the given value to a [`Regex`].
    ///
    /// Returns an [`Error`] if conversion fails.
    fn to_regex(&self) -> Result<Regex, Error>;
}

// Identity conversions

impl ToRegex for Regex {
    fn to_regex(&self) -> Result<Regex, Error> {
        Ok(self.clone())
    }
}

impl ToRegex for &Regex {
    fn to_regex(&self) -> Result<Regex, Error> {
        Ok((*self).clone())
    }
}

// Builder Conversions

impl ToRegex for RegexBuilder {
    fn to_regex(&self) -> Result<Regex, Error> {
        self.build()
    }
}

impl ToRegex for &RegexBuilder {
    fn to_regex(&self) -> Result<Regex, Error> {
        self.build()
    }
}

// String conversions

impl ToRegex for String {
    fn to_regex(&self) -> Result<Regex, Error> {
        Regex::new(self)
    }
}

impl ToRegex for &String {
    fn to_regex(&self) -> Result<Regex, Error> {
        Regex::new(self)
    }
}

impl ToRegex for &str {
    fn to_regex(&self) -> Result<Regex, Error> {
        Regex::new(self)
    }
}

/// A trait for converting a value to a [`RegexSet`].
pub trait ToRegexSet {
    /// Converts the given values to a [`RegexSet`].
    ///
    /// When converting from a [`Regex`] or [`RegexBuilder`],
    /// resets match flags and limits to the defaults.
    /// Use a [`RegexSet`] or [`RegexSetBuilder`] to preserve these settings.
    ///
    /// Returns an [`Error`] if any conversion fails.
    fn to_regex_set(&self) -> Result<RegexSet, Error>;
}

// Identity conversions

impl ToRegexSet for RegexSet {
    fn to_regex_set(&self) -> Result<RegexSet, Error> {
        Ok(self.clone())
    }
}

impl ToRegexSet for &RegexSet {
    fn to_regex_set(&self) -> Result<RegexSet, Error> {
        Ok((*self).clone())
    }
}

// Builder Conversions

impl ToRegexSet for RegexSetBuilder {
    fn to_regex_set(&self) -> Result<RegexSet, Error> {
        self.build()
    }
}

impl ToRegexSet for &RegexSetBuilder {
    fn to_regex_set(&self) -> Result<RegexSet, Error> {
        self.build()
    }
}

// Single item conversion

impl<T> ToRegexSet for T
where
    T: ToRegex,
{
    fn to_regex_set(&self) -> Result<RegexSet, Error> {
        let regex = self.to_regex()?;

        // This conversion discards flags and limits from Regex and RegexBuilder.
        let regex = regex.as_str();

        RegexSet::new(iter::once(regex))
    }
}

/// A trait for collecting an iterator into a [`RegexSet`].
pub trait CollectRegexSet {
    /// Collects the iterator values to a [`RegexSet`].
    ///
    /// When converting from a [`Regex`] or [`RegexBuilder`],
    /// resets match flags and limits to the defaults.
    ///
    /// Use a [`RegexSet`] or [`RegexSetBuilder`] to preserve these settings,
    /// via the `*_regex_set` methods.
    ///
    /// Returns an [`Error`] if any conversion fails.
    fn collect_regex_set(self) -> Result<RegexSet, Error>;
}

// Multi item conversion

impl<I> CollectRegexSet for I
where
    I: IntoIterator,
    I::Item: ToRegex,
{
    fn collect_regex_set(self) -> Result<RegexSet, Error> {
        let regexes: Result<Vec<Regex>, Error> =
            self.into_iter().map(|item| item.to_regex()).collect();
        let regexes = regexes?;

        // This conversion discards flags and limits from Regex and RegexBuilder.
        let regexes = regexes.iter().map(|regex| regex.as_str());

        RegexSet::new(regexes)
    }
}

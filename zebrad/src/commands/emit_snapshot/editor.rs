//! Idempotent, structured editors for the Zebra source constants that the
//! `emit-snapshot` release-time updater regenerates.
//!
//! Each function takes the current source text of a constant module and returns
//! the edited source plus a list of human-readable changes. The editors are
//! **structured**: they locate a named `const` item, then rewrite only the
//! relevant field inside that item's body, so unrelated constants and the
//! surrounding documentation are never touched.
//!
//! Every editor is **idempotent**: re-running it against already-current source
//! produces byte-identical output and an empty change list, so `emit-snapshot`
//! is a no-op once the constants match the chain.
//!
//! These functions are pure (string in, string out) so they are unit-testable
//! without a chain (see the tests at the bottom of this module).

use std::fmt::Write as _;

use color_eyre::eyre::{eyre, Result};

/// A single human-readable change made by an editor, for the review diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// The name of the field or constant that changed.
    pub field: String,
    /// The value before the edit.
    pub old: String,
    /// The value after the edit.
    pub new: String,
}

impl std::fmt::Display for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "  {}: {} -> {}", self.field, self.old, self.new)
    }
}

/// Finds the byte range of the `{ ... }` struct body that follows the
/// `const <name>: ... = ` item in `source`, returning `(open_brace, close_brace)`
/// byte indices (the indices of the `{` and the matching `}`).
///
/// Matches braces while skipping those inside string literals, so the
/// `chunk_hashes` array's brackets and quoted hashes don't confuse the scan.
fn const_body_braces(source: &str, name: &str) -> Result<(usize, usize)> {
    let needle = format!("const {name}");
    let item_start = source
        .find(&needle)
        .ok_or_else(|| eyre!("could not find `const {name}` in the source"))?;

    let open = source[item_start..]
        .find('{')
        .map(|rel| item_start + rel)
        .ok_or_else(|| eyre!("could not find the opening brace of `const {name}`"))?;

    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut i = open;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else {
            match c {
                '"' => in_string = true,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok((open, i));
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }

    Err(eyre!("unbalanced braces in `const {name}`"))
}

/// Replaces the integer argument of a `field: block::Height(N)` assignment
/// inside the body of `const <const_name>` with `new_height`.
///
/// Idempotent: if the height already equals `new_height`, the source is
/// returned unchanged and no [`Change`] is recorded.
pub fn set_height_field(
    source: &str,
    const_name: &str,
    field: &str,
    new_height: u32,
) -> Result<(String, Option<Change>)> {
    let (open, close) = const_body_braces(source, const_name)?;
    let body = &source[open..=close];

    // Locate `<field>:` inside the body, then the `block::Height(` that follows
    // and its closing `)`. Scoped to the body so other heights are untouched.
    let field_needle = format!("{field}:");
    let field_rel = body
        .find(&field_needle)
        .ok_or_else(|| eyre!("could not find field `{field}` in `const {const_name}`"))?;

    let height_marker = "block::Height(";
    let height_rel = body[field_rel..]
        .find(height_marker)
        .map(|rel| field_rel + rel + height_marker.len())
        .ok_or_else(|| {
            eyre!("could not find `block::Height(` for field `{field}` in `const {const_name}`")
        })?;

    let close_paren_rel = body[height_rel..]
        .find(')')
        .map(|rel| height_rel + rel)
        .ok_or_else(|| eyre!("unterminated `block::Height(` for field `{field}`"))?;

    let old_digits: String = body[height_rel..close_paren_rel]
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();
    let old_value: u32 = old_digits
        .parse()
        .map_err(|_| eyre!("could not parse the current height for field `{field}`"))?;

    if old_value == new_height {
        return Ok((source.to_owned(), None));
    }

    let new_literal = group_digits(new_height);
    let mut edited = String::with_capacity(source.len());
    edited.push_str(&source[..open]);
    edited.push_str(&body[..height_rel]);
    edited.push_str(&new_literal);
    edited.push_str(&body[close_paren_rel..]);
    edited.push_str(&source[close + 1..]);

    Ok((
        edited,
        Some(Change {
            field: format!("{const_name}.{field}"),
            old: group_digits(old_value),
            new: new_literal,
        }),
    ))
}

/// Replaces the `chunk_hashes: &[ ... ]` array inside the body of
/// `const <const_name>` with `hashes`, one lowercase-hex string per chunk, in
/// chunk order.
///
/// Idempotent: if the existing array already equals `hashes`, the source is
/// returned unchanged and no [`Change`] is recorded.
pub fn set_chunk_hashes(
    source: &str,
    const_name: &str,
    hashes: &[String],
) -> Result<(String, Option<Change>)> {
    let (open, close) = const_body_braces(source, const_name)?;
    let body = &source[open..=close];

    let field_needle = "chunk_hashes:";
    let field_rel = body
        .find(field_needle)
        .ok_or_else(|| eyre!("could not find field `chunk_hashes` in `const {const_name}`"))?;

    // The value is `&[ ... ]`: find the `[` after the field, then its matching
    // `]` (skipping brackets inside string literals — hashes have none, but be
    // robust).
    let open_bracket_rel = body[field_rel..]
        .find('[')
        .map(|rel| field_rel + rel)
        .ok_or_else(|| eyre!("could not find `[` for `chunk_hashes` in `const {const_name}`"))?;

    let close_bracket_rel = matching_bracket(body, open_bracket_rel)?;

    let existing = &body[open_bracket_rel + 1..close_bracket_rel];
    let existing_hashes = parse_hash_array(existing);

    if existing_hashes == hashes {
        return Ok((source.to_owned(), None));
    }

    // Render the new array body. The field is indented 4 spaces (struct field),
    // its entries 8 spaces, matching the existing formatting and `rustfmt`.
    let mut rendered = String::from("\n");
    for hash in hashes {
        let _ = writeln!(rendered, "        \"{hash}\",");
    }
    rendered.push_str("    ");

    let mut edited = String::with_capacity(source.len() + rendered.len());
    edited.push_str(&source[..open]);
    edited.push_str(&body[..=open_bracket_rel]);
    edited.push_str(&rendered);
    edited.push_str(&body[close_bracket_rel..]);
    edited.push_str(&source[close + 1..]);

    Ok((
        edited,
        Some(Change {
            field: format!("{const_name}.chunk_hashes"),
            old: format!("{} chunks", existing_hashes.len()),
            new: format!(
                "{} chunks (last: {})",
                hashes.len(),
                hashes.last().map(String::as_str).unwrap_or("<none>"),
            ),
        }),
    ))
}

/// Replaces the integer argument of a standalone
/// `pub const <name>: Height = Height(N);` declaration with `new_height`.
///
/// Used for the checkpoint max-height constants, which are standalone items
/// rather than struct fields. Idempotent: an unchanged height is a no-op.
pub fn set_standalone_height(
    source: &str,
    name: &str,
    new_height: u32,
) -> Result<(String, Option<Change>)> {
    let decl = format!("const {name}");
    let decl_start = source
        .find(&decl)
        .ok_or_else(|| eyre!("could not find `const {name}` in the source"))?;

    let marker = "Height(";
    let height_start = source[decl_start..]
        .find(marker)
        .map(|rel| decl_start + rel + marker.len())
        .ok_or_else(|| eyre!("could not find `Height(` for `const {name}`"))?;

    let close_paren = source[height_start..]
        .find(')')
        .map(|rel| height_start + rel)
        .ok_or_else(|| eyre!("unterminated `Height(` for `const {name}`"))?;

    let old_digits: String = source[height_start..close_paren]
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();
    let old_value: u32 = old_digits
        .parse()
        .map_err(|_| eyre!("could not parse the current height for `const {name}`"))?;

    if old_value == new_height {
        return Ok((source.to_owned(), None));
    }

    let new_literal = group_digits(new_height);
    let mut edited = String::with_capacity(source.len());
    edited.push_str(&source[..height_start]);
    edited.push_str(&new_literal);
    edited.push_str(&source[close_paren..]);

    Ok((
        edited,
        Some(Change {
            field: name.to_owned(),
            old: group_digits(old_value),
            new: new_literal,
        }),
    ))
}

/// Sets a standalone `pub const <name>: &str = "..."` value, inserting the whole
/// item (with a doc comment) before `anchor_const` if it does not exist yet.
///
/// Used for the `*_UNSPENT_OUTPUTS_HASH` and `*_ADDRESS_BALANCES_HASH`
/// constants, which are added near the known-hash specs. Idempotent: an existing
/// item with the same value is left unchanged.
pub fn set_or_insert_str_const(
    source: &str,
    name: &str,
    value: &str,
    doc: &str,
    anchor_const: &str,
) -> Result<(String, Option<Change>)> {
    let decl = format!("pub const {name}: &str =");

    if let Some(decl_start) = source.find(&decl) {
        // Replace the existing string literal value.
        let after_decl = decl_start + decl.len();
        let open_quote = source[after_decl..]
            .find('"')
            .map(|rel| after_decl + rel)
            .ok_or_else(|| eyre!("could not find the value of `const {name}`"))?;
        let close_quote = source[open_quote + 1..]
            .find('"')
            .map(|rel| open_quote + 1 + rel)
            .ok_or_else(|| eyre!("unterminated string literal for `const {name}`"))?;

        let old_value = &source[open_quote + 1..close_quote];
        if old_value == value {
            return Ok((source.to_owned(), None));
        }

        let mut edited = String::with_capacity(source.len());
        edited.push_str(&source[..=open_quote]);
        edited.push_str(value);
        edited.push_str(&source[close_quote..]);

        return Ok((
            edited,
            Some(Change {
                field: name.to_owned(),
                old: old_value.to_owned(),
                new: value.to_owned(),
            }),
        ));
    }

    // Insert a fresh item immediately before the anchor const's doc comment /
    // declaration. Find the start of the line that begins the anchor item,
    // including any leading `///` doc lines, by walking back over doc-comment
    // lines.
    let anchor_decl = format!("const {anchor_const}");
    let anchor_start = source
        .find(&anchor_decl)
        .ok_or_else(|| eyre!("could not find anchor `const {anchor_const}` to insert `{name}`"))?;

    // Walk back to the first line of the anchor's leading doc block (lines that,
    // when trimmed, start with `///` or `pub`), so the new item lands above the
    // whole documented item.
    let insert_at = line_start_of_item(source, anchor_start);

    let mut item = String::new();
    for doc_line in doc.lines() {
        let _ = writeln!(item, "/// {doc_line}");
    }
    let _ = writeln!(item, "pub const {name}: &str = \"{value}\";");
    item.push('\n');

    let mut edited = String::with_capacity(source.len() + item.len());
    edited.push_str(&source[..insert_at]);
    edited.push_str(&item);
    edited.push_str(&source[insert_at..]);

    Ok((
        edited,
        Some(Change {
            field: name.to_owned(),
            old: "<absent>".to_owned(),
            new: value.to_owned(),
        }),
    ))
}

/// Returns the byte index of the start of the documented item that contains
/// `item_decl_start` (the index of `pub const`/`const`), walking back over
/// contiguous leading `///` doc-comment lines so an inserted item lands above
/// the whole documented block.
fn line_start_of_item(source: &str, item_decl_start: usize) -> usize {
    // Start of the declaration's own line.
    let mut line_start = source[..item_decl_start]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(0);

    // Walk backwards over preceding doc-comment lines.
    loop {
        if line_start == 0 {
            break;
        }
        // The previous line is `source[prev_line_start..line_start-1]`.
        let prev_line_start = source[..line_start - 1]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let prev_line = source[prev_line_start..line_start - 1].trim_start();
        if prev_line.starts_with("///") {
            line_start = prev_line_start;
        } else {
            break;
        }
    }

    line_start
}

/// Returns the byte index of the `]` matching the `[` at `open` in `s`,
/// skipping brackets inside string literals.
fn matching_bracket(s: &str, open: usize) -> Result<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut i = open;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else {
            match c {
                '"' => in_string = true,
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    Err(eyre!("unbalanced brackets starting at byte {open}"))
}

/// Parses the contents of a `chunk_hashes` array body (the text between `[` and
/// `]`) into the ordered list of quoted lowercase-hex strings.
fn parse_hash_array(body: &str) -> Vec<String> {
    let mut hashes = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find('"') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('"') else { break };
        hashes.push(after[..close].to_owned());
        rest = &after[close + 1..];
    }
    hashes
}

/// Formats `value` with `_` digit separators every three digits, matching the
/// Rust source style used for heights (e.g. `3_373_206`).
fn group_digits(value: u32) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let len = digits.len();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push('_');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests;

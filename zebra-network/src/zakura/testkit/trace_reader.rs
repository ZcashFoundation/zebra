//! Post-flush JSONL trace reader for Zakura tests.

use std::{
    fs,
    io::{self, BufRead},
    path::{Path, PathBuf},
};

use serde_json::Value;

/// Loaded Zakura trace tables.
#[derive(Clone, Debug, Default)]
pub struct TraceReader {
    rows: Vec<TraceRow>,
}

#[derive(Clone, Debug)]
struct TraceRow {
    table: String,
    source_node: Option<String>,
    row: Value,
}

/// A filtered view over trace rows.
#[derive(Clone, Debug)]
pub struct TraceQuery<'a> {
    reader: &'a TraceReader,
    table: Option<&'a str>,
    node: Option<&'a str>,
}

impl TraceReader {
    /// Load all `*.jsonl` files in `path` and one level of per-node
    /// subdirectories.
    pub fn load(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let mut reader = Self::default();
        if !path.exists() {
            return Ok(reader);
        }

        reader.load_dir(path, None)?;
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let source_node = source_node_from_dir(&entry.path());
                reader.load_dir(&entry.path(), source_node)?;
            }
        }

        Ok(reader)
    }

    /// Filter rows to a logical table.
    pub fn table<'a>(&'a self, table: &'a str) -> TraceQuery<'a> {
        TraceQuery {
            reader: self,
            table: Some(table),
            node: None,
        }
    }

    /// Filter rows to a node label.
    pub fn node<'a>(&'a self, node: &'a str) -> TraceQuery<'a> {
        TraceQuery {
            reader: self,
            table: None,
            node: Some(node),
        }
    }

    /// Return all loaded rows.
    pub fn rows(&self) -> Vec<&Value> {
        self.rows.iter().map(|row| &row.row).collect()
    }

    fn load_dir(&mut self, path: &Path, source_node: Option<String>) -> io::Result<()> {
        let mut files = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        files.sort_by_key(|entry| entry.path());

        for entry in files {
            let path = entry.path();
            if !entry.file_type()?.is_file()
                || path.extension().and_then(|ext| ext.to_str()) != Some("jsonl")
            {
                continue;
            }

            self.load_file(path, source_node.clone())?;
        }

        Ok(())
    }

    fn load_file(&mut self, path: PathBuf, source_node: Option<String>) -> io::Result<()> {
        let table = path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid trace file name"))?
            .to_string();
        let file = fs::File::open(path)?;

        for line in io::BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let row = serde_json::from_str(&line)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            self.rows.push(TraceRow {
                table: table.clone(),
                source_node: source_node.clone(),
                row,
            });
        }

        Ok(())
    }
}

impl<'a> TraceQuery<'a> {
    /// Narrow this query to a logical table.
    pub fn table(mut self, table: &'a str) -> Self {
        self.table = Some(table);
        self
    }

    /// Narrow this query to a node label.
    pub fn node(mut self, node: &'a str) -> Self {
        self.node = Some(node);
        self
    }

    fn matching(&self) -> impl Iterator<Item = &'a TraceRow> + '_ {
        self.reader.rows.iter().filter(|row| {
            self.table.is_none_or(|table| row.table == table)
                && self.node.is_none_or(|node| row.matches_node(node))
        })
    }

    /// Return matching rows in file order.
    pub fn rows(&self) -> Vec<&'a Value> {
        self.matching().map(|row| &row.row).collect()
    }

    /// Count matching rows whose `event` field equals `event`.
    pub fn count(&self, event: &str) -> usize {
        self.matching()
            .map(|row| &row.row)
            .filter(|row| row.get("event").and_then(Value::as_str) == Some(event))
            .count()
    }

    /// Return the first matching row.
    pub fn first(&self) -> Option<&'a Value> {
        self.matching().map(|row| &row.row).next()
    }

    /// Return the last matching row.
    pub fn last(&self) -> Option<&'a Value> {
        self.matching().map(|row| &row.row).last()
    }

    /// Assert that `events` appears as an ordered subsequence in this query.
    ///
    /// Use this for one table and one node. Cross-table and cross-node ordering
    /// is intentionally not modeled by the batched JSONL writer.
    pub fn assert_sequence(&self, events: &[&str]) {
        let mut next = 0;

        for row in self.matching().map(|row| &row.row) {
            if next == events.len() {
                break;
            }
            if row.get("event").and_then(Value::as_str) == Some(events[next]) {
                next += 1;
            }
        }

        assert_eq!(
            next,
            events.len(),
            "trace did not contain expected event subsequence: {events:?}",
        );
    }
}

impl TraceRow {
    fn matches_node(&self, node: &str) -> bool {
        if let Some(source_node) = &self.source_node {
            return source_node == node;
        }

        self.row
            .get("node")
            .and_then(Value::as_str)
            .is_some_and(|row_node| row_node == node)
    }
}

fn source_node_from_dir(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("node-"))
        .filter(|node| !node.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_counts_and_matches_subsequences_within_a_table() {
        let dir = tempfile::tempdir().expect("tempdir");
        let node_dir = dir.path().join("node-01");
        fs::create_dir_all(&node_dir).expect("node dir");
        fs::write(
            node_dir.join("handshake.jsonl"),
            r#"{"node":"01","event":"control.started"}"#.to_string()
                + "\n"
                + r#"{"node":"01","event":"control.succeeded"}"#
                + "\n",
        )
        .expect("trace file");

        let reader = TraceReader::load(dir.path()).expect("reader");
        assert_eq!(
            reader
                .node("01")
                .table("handshake")
                .count("control.started"),
            1
        );
        reader
            .node("01")
            .table("handshake")
            .assert_sequence(&["control.started", "control.succeeded"]);
    }

    #[test]
    fn reader_uses_node_subdir_before_row_node_field() {
        let dir = tempfile::tempdir().expect("tempdir");
        let node_dir = dir.path().join("node-01");
        fs::create_dir_all(&node_dir).expect("node dir");
        fs::write(
            node_dir.join("conn.jsonl"),
            r#"{"node":"wrong","event":"accepted"}"#.to_string() + "\n",
        )
        .expect("trace file");

        let reader = TraceReader::load(dir.path()).expect("reader");
        assert_eq!(reader.node("01").table("conn").count("accepted"), 1);
        assert_eq!(reader.node("wrong").table("conn").count("accepted"), 0);
    }
}

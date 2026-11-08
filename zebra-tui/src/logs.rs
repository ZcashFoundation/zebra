//! Follows a growing log file and yields its new lines.

use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::PathBuf,
};

/// The number of trailing bytes to read when a log file is first opened.
const INITIAL_TAIL_BYTES: u64 = 256 * 1024;

/// Reads lines that are appended to a log file.
///
/// If the file shrinks, for example after rotation or truncation, reading restarts at its start.
pub struct LogTail {
    path: PathBuf,
    position: Option<u64>,
    partial_line: Vec<u8>,
}

impl LogTail {
    /// Creates a reader for `path`. The first poll reads only the end of the file.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            position: None,
            partial_line: Vec::new(),
        }
    }

    /// Returns the complete lines written since the previous poll, without ANSI escape sequences.
    pub fn poll(&mut self) -> io::Result<Vec<String>> {
        let mut file = File::open(&self.path)?;
        let length = file.metadata()?.len();

        let (start, skip_first_line) = match self.position {
            None => {
                let start = length.saturating_sub(INITIAL_TAIL_BYTES);
                (start, start > 0)
            }
            Some(position) if length < position => {
                self.partial_line.clear();
                (0, false)
            }
            Some(position) => (position, false),
        };

        file.seek(SeekFrom::Start(start))?;
        let mut bytes = std::mem::take(&mut self.partial_line);
        let read = file.read_to_end(&mut bytes)?;
        // `read` is at most the file length, which fits in u64.
        self.position = Some(start + read as u64);

        let mut lines: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
        self.partial_line = lines.pop().map(<[u8]>::to_vec).unwrap_or_default();
        let lines = lines
            .into_iter()
            .skip(usize::from(skip_first_line))
            .map(|line| strip_ansi(&String::from_utf8_lossy(line)))
            .collect();
        Ok(lines)
    }
}

/// Removes ANSI CSI escape sequences, such as color codes, and trailing carriage returns.
pub fn strip_ansi(line: &str) -> String {
    let mut stripped = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for terminator in chars.by_ref() {
                if ('@'..='~').contains(&terminator) {
                    break;
                }
            }
        } else {
            stripped.push(c);
        }
    }
    stripped.trim_end_matches('\r').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn strips_color_codes() {
        assert_eq!(
            strip_ansi("\u{1b}[2m2026\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m zebrad: ok\r"),
            "2026  INFO zebrad: ok"
        );
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn yields_only_complete_new_lines_and_restarts_after_truncation() {
        let path = std::env::temp_dir().join(format!("zebra-tui-test-{}.log", std::process::id()));
        std::fs::write(&path, "one\ntwo\npart").expect("temp dir is writable");
        let mut tail = LogTail::new(path.clone());
        assert_eq!(tail.poll().expect("file exists"), vec!["one", "two"]);

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("file exists");
        file.write_all(b"ial\nthree\n").expect("file is writable");
        assert_eq!(tail.poll().expect("file exists"), vec!["partial", "three"]);
        assert!(tail.poll().expect("file exists").is_empty());

        std::fs::write(&path, "new\n").expect("temp dir is writable");
        assert_eq!(tail.poll().expect("file exists"), vec!["new"]);
        let _ = std::fs::remove_file(&path);
    }
}

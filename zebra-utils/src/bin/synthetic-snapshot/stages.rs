//! Benchmark intervals from stock zebrad's timestamped bootstrap logs.

use std::{
    fs::File,
    io::{self, BufRead, BufReader},
    path::Path,
};

use chrono::{DateTime, FixedOffset};
use serde::Serialize;

const PREFIX: &str = ": state snapshot stage: ";
const MARKERS: [&str; 5] = [
    "manifest verified",
    "last piece verified",
    "SST files built",
    "state renamed into place",
    "state initialized",
];
type Boundary = Option<(usize, DateTime<FixedOffset>)>;

#[derive(Default, Serialize)]
pub(super) struct StageTimings {
    pub manifest_verified_to_last_piece_verified: Option<f64>,
    pub last_piece_verified_to_sst_files_built: Option<f64>,
    pub sst_files_built_to_state_renamed_into_place: Option<f64>,
    pub state_renamed_into_place_to_state_initialized: Option<f64>,
}

impl StageTimings {
    pub(super) fn read(path: &Path) -> io::Result<Self> {
        Self::parse(BufReader::new(File::open(path)?))
    }

    fn parse(reader: impl BufRead) -> io::Result<Self> {
        let mut timestamps = [None; MARKERS.len()];
        let mut seen = [false; MARKERS.len()];
        for (index, line) in reader.lines().enumerate() {
            let line = line?;
            let Some((header, message)) = line.split_once(PREFIX) else {
                continue;
            };
            let Some(stage) = MARKERS.iter().position(|marker| {
                message
                    .strip_prefix(marker)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
            }) else {
                continue;
            };
            timestamps[stage] = if seen[stage] {
                None // Duplicate boundaries cannot be attributed to one bootstrap.
            } else {
                header
                    .split_ascii_whitespace()
                    .next()
                    .and_then(|time| DateTime::parse_from_rfc3339(time).ok())
                    .map(|time| (index, time))
            };
            seen[stage] = true;
        }
        // The file order is checked separately: timestamps come from a wall clock.
        fn interval(start: Boundary, end: Boundary) -> Option<f64> {
            let (first, start) = start?;
            let (last, end) = end?;
            if first >= last {
                return None;
            }
            let duration = (end - start).to_std().ok()?;
            Some(duration.as_secs_f64())
        }
        let [manifest, pieces, built, renamed, initialized] = timestamps;
        Ok(Self {
            manifest_verified_to_last_piece_verified: interval(manifest, pieces),
            last_piece_verified_to_sst_files_built: interval(pieces, built),
            sst_files_built_to_state_renamed_into_place: interval(built, renamed),
            state_renamed_into_place_to_state_initialized: interval(renamed, initialized),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{io, StageTimings};

    #[test]
    fn stage_intervals_require_unambiguous_ordered_boundaries() -> io::Result<()> {
        let log = "2026-11-23T22:25:00.000Z INFO zebrad: state snapshot stage: manifest verified pieces=24\n\
                   2026-11-23T22:25:01.500Z INFO zebrad: state snapshot stage: last piece verified pieces=24\n\
                   2026-11-23T22:25:02.000Z INFO zebrad: state snapshot stage: SST files built ssts=24\n\
                   2026-11-23T22:25:03.000Z INFO zebrad: state snapshot stage: SST files built ssts=24\n\
                   2026-11-23T22:25:04.000Z INFO zebrad: state snapshot stage: state renamed into place\n\
                   2026-11-23T22:25:03.000Z INFO zebrad: state snapshot stage: state initialized\n";
        let timings = StageTimings::parse(log.as_bytes())?;
        assert_eq!(
            serde_json::to_value(timings)?,
            serde_json::json!({
                "manifest_verified_to_last_piece_verified": 1.5,
                "last_piece_verified_to_sst_files_built": null,
                "sst_files_built_to_state_renamed_into_place": null,
                "state_renamed_into_place_to_state_initialized": null,
            })
        );
        Ok(())
    }
}

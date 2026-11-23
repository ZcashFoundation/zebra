//! Compare two zebrad builds against the same prepared snapshot.

use std::{fs, path::PathBuf};
use zebra_state::BoxError;

use super::{report::TrialReport, run};

#[derive(Debug, clap::Args)]
pub(super) struct Args {
    /// JSON configuration previously resolved by prepare.
    #[arg(long)]
    config: PathBuf,
    /// Baseline zebrad executable; also used to serve both runs.
    #[arg(long)]
    baseline: PathBuf,
    /// Candidate zebrad executable to benchmark as a bootstrap node.
    #[arg(long)]
    candidate: PathBuf,
    /// New directory for both runs' reports, databases, and logs.
    #[arg(long)]
    output: PathBuf,
}

pub(super) fn run(args: Args) -> Result<(), BoxError> {
    let config = args.config.canonicalize()?;
    let baseline = args.baseline.canonicalize()?;
    let candidate = args.candidate.canonicalize()?;
    fs::create_dir(&args.output)?;
    let baseline_output = args.output.join("baseline");
    let candidate_output = args.output.join("candidate");
    eprintln!("Baseline: {}", baseline.display());
    let before = run::command(&config, &baseline, &baseline, &baseline_output, false)?;
    eprintln!("Candidate: {}", candidate.display());
    let after = run::command(&config, &baseline, &candidate, &candidate_output, false)?;
    compare(&before, &after);
    eprintln!(
        "baseline report: {}",
        baseline_output.join("report.json").display()
    );
    eprintln!(
        "candidate report: {}",
        candidate_output.join("report.json").display()
    );
    Ok(())
}

type Measure = fn(&TrialReport) -> Option<f64>;

fn compare(before: &[TrialReport], after: &[TrialReport]) {
    let measurements: [(&str, Measure); 5] = [
        ("Bootstrap (harness)", |trial| {
            trial.timings_seconds.bootstrap
        }),
        ("manifest verified → last piece verified", |trial| {
            trial
                .bootstrap_stages_seconds
                .manifest_verified_to_last_piece_verified
        }),
        ("last piece verified → SST files built", |trial| {
            trial
                .bootstrap_stages_seconds
                .last_piece_verified_to_sst_files_built
        }),
        ("SST files built → state renamed into place", |trial| {
            trial
                .bootstrap_stages_seconds
                .sst_files_built_to_state_renamed_into_place
        }),
        ("state renamed into place → state initialized", |trial| {
            trial
                .bootstrap_stages_seconds
                .state_renamed_into_place_to_state_initialized
        }),
    ];
    println!(
        "\n{} correctness-checked trials per build; median seconds (candidate − baseline)",
        before.len()
    );
    println!(
        "{:<45} {:>12} {:>12} {:>12}",
        "Interval", "Baseline", "Candidate", "Delta"
    );
    for (label, measure) in measurements {
        let baseline = median(before, measure);
        let candidate = median(after, measure);
        let seconds =
            |value: Option<f64>| value.map_or_else(|| "n/a".to_owned(), |n| format!("{n:.6}"));
        let delta = baseline
            .zip(candidate)
            .map_or_else(|| "n/a".to_owned(), |(a, b)| format!("{:+.6}", b - a));
        println!(
            "{label:<45} {:>12} {:>12} {delta:>12}",
            seconds(baseline),
            seconds(candidate)
        );
    }
}

fn median(trials: &[TrialReport], measure: Measure) -> Option<f64> {
    let mut samples: Vec<_> = trials.iter().map(measure).collect::<Option<_>>()?;
    samples.sort_unstable_by(f64::total_cmp);
    let center = samples.len() / 2;
    let upper = *samples.get(center)?;
    Some(if samples.len() % 2 == 0 {
        (samples[center - 1] + upper) / 2.0
    } else {
        upper
    })
}

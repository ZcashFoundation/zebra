//! Select small, reachable corpora from the original mainnet transaction files.
//! Usage: prepare_equivalence_corpus <seeds-real directory> <empty output directory>
//! Outputs are candidates: replay and coverage-minimise before packaging.
use std::{env, fs, path::Path};
use zebra_fuzz_targets::equivalence::{orchard, tower};

fn write(dest: &Path, prefix: &str, name: &str, data: &[u8], control: u8) {
    let mut seed = data.to_vec();
    seed.push(control);
    fs::write(dest.join(format!("{prefix}_{name}")), seed).unwrap();
}

fn main() {
    let args: Vec<_> = env::args().collect();
    assert_eq!(args.len(), 3, "expected source and empty destination");
    let source = Path::new(&args[1]);
    let dest = Path::new(&args[2]);
    assert!(!dest.exists(), "destination must not already exist");
    let od = dest.join("orchard_batch_equivalence");
    let td = dest.join("tower_partition_equivalence");
    fs::create_dir_all(&od).unwrap();
    fs::create_dir_all(&td).unwrap();
    for (era, dir) in [
        (0, "orchard_v5_pre_nu6_2"),
        (1, "orchard_v5_nu6_2"),
        (2, "nu6_3_activation"),
    ] {
        let mut paths: Vec<_> = fs::read_dir(source.join(dir))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "bin"))
            .collect();
        paths.sort();
        let mut orchard_candidates = Vec::new();
        let mut tower_candidates = Vec::new();
        for path in paths {
            let data = fs::read(&path).unwrap();
            let oi = orchard::items(&data);
            if !oi.is_empty() {
                orchard_candidates.push((
                    path.clone(),
                    data.clone(),
                    oi.iter().any(|i| i.ironwood),
                ));
            }
            let ti = tower::items(&data);
            if tower::partition(ti.len(), 0).is_some() {
                tower_candidates.push((path, data));
            }
        }
        // Evenly distributed samples, not a prefix of the collection window.
        let step = orchard_candidates.len().div_ceil(8).max(1);
        for (path, data, _) in orchard_candidates.iter().step_by(step) {
            write(
                &od,
                &format!("era{era}"),
                path.file_name().unwrap().to_str().unwrap(),
                data,
                era,
            );
        }
        // Ensure the new pool is retained even when the regular stride misses it.
        if era == 2 {
            if let Some((path, data, _)) =
                orchard_candidates.iter().find(|(_, _, ironwood)| *ironwood)
            {
                write(
                    &od,
                    "ironwood",
                    path.file_name().unwrap().to_str().unwrap(),
                    data,
                    era,
                );
            }
        }
        let step = tower_candidates.len().div_ceil(8).max(1);
        for (path, data) in tower_candidates.iter().step_by(step) {
            write(
                &td,
                &format!("era{era}"),
                path.file_name().unwrap().to_str().unwrap(),
                data,
                0,
            );
        }
        println!(
            "{dir}: reachable orchard={} tower={}",
            orchard_candidates.len(),
            tower_candidates.len()
        );
    }
}

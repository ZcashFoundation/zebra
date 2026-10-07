//! Replay a seed directory and fail if any entry misses the target's assertions.
//! Usage: cargo run --release --example check_equivalence_corpus -- <orchard|tower> <dir>
use std::{env, fs};
use zebra_fuzz_targets::equivalence::{orchard, seed, tower};

fn main() {
    let args: Vec<_> = env::args().collect();
    assert_eq!(args.len(), 3, "expected target family and seed directory");
    let mut paths: Vec<_> = fs::read_dir(&args[2])
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    paths.sort();
    let mut accepted = [0usize; 3];
    let mut count = 0;
    let mut ironwood = 0;
    let mut mixed = 0;
    let mut rejected_bad_sighashes = 0;
    for path in paths {
        assert!(path.is_file(), "seed directory contains a non-file");
        let data = fs::read(&path).unwrap();
        let (&control, stream) = data.split_last().expect("nonempty seed");
        let (reached, valid) = match args[1].as_str() {
            "orchard" => {
                let reached = orchard::check(&data);
                let mut items = orchard::items(stream);
                items.retain(|item| match control >> 6 {
                    1 => !item.ironwood,
                    2 => item.ironwood,
                    _ => true,
                });
                let has_ironwood = items.iter().any(|i| i.ironwood);
                ironwood += usize::from(has_ironwood);
                mixed += usize::from(has_ironwood && items.iter().any(|i| !i.ironwood));
                let valid = !items.is_empty()
                    && items
                        .iter()
                        .all(|i| orchard::single(i, &orchard::keys()[usize::from(control) % 3]))
                    && orchard::batch(
                        &items.iter().collect::<Vec<_>>(),
                        &orchard::keys()[usize::from(control) % 3],
                        seed(&data),
                    )
                    .into_iter()
                    .all(|ok| ok);
                if valid {
                    // Keep the mined proof and signatures, but invalidate their
                    // signed message. Both verification paths must reject it.
                    items[0].sighash[0] ^= 1;
                    let key = &orchard::keys()[usize::from(control) % 3];
                    assert!(!orchard::single(&items[0], key));
                    assert!(
                        !orchard::batch(&items.iter().collect::<Vec<_>>(), key, seed(&data),)[0]
                    );
                    rejected_bad_sighashes += 1;
                }
                (reached, valid)
            }
            "tower" => {
                let reached = tower::check(&data);
                let items = tower::items(stream);
                let valid =
                    !items.is_empty() && items.iter().all(|i| i.0.clone().verify_single().is_ok());
                (reached, valid)
            }
            _ => panic!("expected orchard or tower"),
        };
        assert!(reached > 0, "seed misses oracle: {}", path.display());
        if valid {
            accepted[usize::from(control) % 3] += 1;
        }
        println!(
            "{} items={reached} all_valid={valid}",
            path.file_name().unwrap().to_string_lossy()
        );
        count += 1;
    }
    assert!(
        count > 0 && accepted.iter().sum::<usize>() > 0,
        "need nonempty valid seed coverage"
    );
    if args[1] == "orchard" {
        assert!(
            accepted.iter().all(|&n| n > 0),
            "need valid seeds for all three eras"
        );
        assert!(
            ironwood > 0 && mixed > 0,
            "need Ironwood and mixed-pool seeds"
        );
    }
    println!("SUMMARY seeds={count} accepted_by_control_mod3={accepted:?} ironwood={ironwood} mixed={mixed} rejected_bad_sighashes={rejected_bad_sighashes}");
}

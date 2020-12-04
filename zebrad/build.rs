#![allow(clippy::try_err)]

use std::{env, fs, fs::File, io::Read, path::PathBuf};

use vergen::{generate_cargo_keys, ConstantsFlags};

fn main() {
    let mut flags = ConstantsFlags::empty();
    flags.toggle(ConstantsFlags::SHA_SHORT);

    // We want to use REBUILD_ON_HEAD_CHANGE here, but vergen assumes that the
    // git directory is in the crate directory, and Zebra uses a workspace.
    // See rustyhorde/vergen#15 and rustyhorde/vergen#21 for details.
    let result = generate_rebuild_key();
    if let Err(err) = result {
        eprintln!("Error generating 'cargo:rerun-if-changed': {:?}", err);
    }

    // Generate the 'cargo:' key output
    generate_cargo_keys(flags).expect("Unable to generate the cargo keys!");
}

/// Generate the `cargo:` rebuild keys output
///
/// The keys that can be generated include:
/// * `cargo:rustc-rerun-if-changed=<git dir>/HEAD`
/// * `cargo:rustc-rerun-if-changed=<file git HEAD points to>`
fn generate_rebuild_key() -> Result<(), Box<dyn std::error::Error>> {
    // Look for .git and ../.git
    // We should really use the `git2` crate here, see rustyhorde/vergen#15
    let mut git_dir_or_file = env::current_dir()?.join(".git");
    let mut metadata = fs::metadata(&git_dir_or_file);
    // git searches all the ancestors of the current directory, but Zebra's
    // crates are direct children of the workspace directory, so we only
    // need to look at the parent directory
    if metadata.is_err() {
        git_dir_or_file = env::current_dir()?
            .parent()
            .ok_or("finding crate's parent directory")?
            .to_path_buf()
            .join(".git");
        metadata = fs::metadata(&git_dir_or_file);
    }

    // Modified from vergen's REBUILD_ON_HEAD_CHANGE implementation:
    // https://github.com/rustyhorde/vergen/blob/master/src/output/envvar.rs#L46
    if let Ok(metadata) = metadata {
        if metadata.is_dir() {
            // Echo the HEAD path
            let git_head_path = git_dir_or_file.join("HEAD");
            println!("cargo:rerun-if-changed={}", git_head_path.display());

            // Determine where HEAD points and echo that path also.
            let mut f = File::open(&git_head_path)?;
            let mut git_head_contents = String::new();
            let _ = f.read_to_string(&mut git_head_contents)?;
            eprintln!("HEAD contents: {}", git_head_contents);
            let ref_vec: Vec<&str> = git_head_contents.split(": ").collect();

            if ref_vec.len() == 2 {
                let current_head_file = ref_vec[1].trim();
                let git_refs_path = git_dir_or_file.join(current_head_file);
                println!("cargo:rerun-if-changed={}", git_refs_path.display());
            } else {
                eprintln!("You are most likely in a detached HEAD state");
            }
        } else if metadata.is_file() {
            // We are in a worktree, so find out where the actual worktrees/<name>/HEAD file is.
            let mut git_file = File::open(&git_dir_or_file)?;
            let mut git_contents = String::new();
            let _ = git_file.read_to_string(&mut git_contents)?;
            let dir_vec: Vec<&str> = git_contents.split(": ").collect();
            eprintln!(".git contents: {}", git_contents);
            let git_path = dir_vec[1].trim();

            // Echo the HEAD path
            let git_head_path = PathBuf::from(git_path).join("HEAD");
            println!("cargo:rerun-if-changed={}", git_head_path.display());

            // Find out what the full path to the .git dir is.
            let mut actual_git_dir = PathBuf::from(git_path);
            actual_git_dir.pop();
            actual_git_dir.pop();

            // Determine where HEAD points and echo that path also.
            let mut f = File::open(&git_head_path)?;
            let mut git_head_contents = String::new();
            let _ = f.read_to_string(&mut git_head_contents)?;
            eprintln!("HEAD contents: {}", git_head_contents);
            let ref_vec: Vec<&str> = git_head_contents.split(": ").collect();

            if ref_vec.len() == 2 {
                let current_head_file = ref_vec[1].trim();
                let git_refs_path = actual_git_dir.join(current_head_file);
                println!("cargo:rerun-if-changed={}", git_refs_path.display());
            } else {
                eprintln!("You are most likely in a detached HEAD state");
            }
        } else {
            Err("Invalid .git format (Not a directory or a file)")?;
        };
    } else {
        Err(".git directory or file not found in crate dir or parent dir")?;
    };

    Ok(())
}

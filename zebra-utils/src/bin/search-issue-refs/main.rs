//! Recursively searches local directory for references to issues that are closed.
//!
//! Requires a Github access token as this program will make queries to the GitHub API where authentication is needed.
//!
//! See <https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/creating-a-personal-access-token>
//!
//! Example usage:
//!
//! (from the root directory of the Zebra repo)
//! ```console
//! GITHUB_TOKEN={valid_github_access_token} search-issue-refs
//! ```
//!
//! Example output:
//!
//! > Found 3 possible issue refs, checking Github issue statuses..
//! >
//! > --------------------------------------
//! > Found reference to closed issue #4794: ./zebra-rpc/src/methods/get_block_template_rpcs.rs:114:19
//! > <https://github.com/ZcashFoundation/zebra/blob/main/zebra-rpc/src/methods/get_block_template_rpcs.rs#L114>
//! > <https://github.com/ZcashFoundation/zebra/issues/4794>
//! >
//! > --------------------------------------
//! > Found reference to closed issue #2379: ./zebra-consensus/src/transaction.rs:717:49
//! > <https://github.com/ZcashFoundation/zebra/blob/main/zebra-consensus/src/transaction.rs#L717>
//! > <https://github.com/ZcashFoundation/zebra/issues/2379>
//! >
//! > --------------------------------------
//! > Found reference to closed issue #3027: ./zebra-consensus/src/transaction/check.rs:319:6
//! > <https://github.com/ZcashFoundation/zebra/blob/main/zebra-consensus/src/transaction/check.rs#L319>
//! > <https://github.com/ZcashFoundation/zebra/issues/3027>
//! >
//! > Found 3 references to closed issues.

use std::{
    env,
    ffi::OsStr,
    fs::{self, File},
    io::{self, BufRead},
    path::PathBuf,
};

use color_eyre::eyre::Result;
use regex::Regex;
use reqwest::{
    header::{self, HeaderMap, HeaderValue},
    ClientBuilder,
};

use tokio::task::JoinSet;
use zebra_utils::init_tracing;

const GITHUB_TOKEN_ENV_KEY: &str = "GITHUB_TOKEN";

const VALID_EXTENSIONS: [&str; 4] = ["rs", "yml", "yaml", "toml"];

fn check_file_ext(ext: &OsStr) -> bool {
    VALID_EXTENSIONS
        .into_iter()
        .any(|valid_extension| valid_extension == ext)
}

fn search_directory(path: &PathBuf) -> Result<Vec<PathBuf>> {
    if path.starts_with("/target/") {
        return Ok(vec![]);
    }

    Ok(fs::read_dir(path)?
        .filter_map(|entry| {
            let path = entry.ok()?.path();

            if path.is_dir() {
                search_directory(&path).ok()
            } else if path.is_file() {
                match path.extension() {
                    Some(ext) if check_file_ext(ext) => Some(vec![path]),
                    _ => None,
                }
            } else {
                None
            }
        })
        .flatten()
        .collect())
}

fn github_issue_url(issue_id: &str) -> String {
    format!("https://github.com/ZcashFoundation/zebra/issues/{issue_id}")
}

fn github_remote_file_ref(file_path: &str, line: usize) -> String {
    let file_path = &file_path[2..];
    format!("https://github.com/ZcashFoundation/zebra/blob/main/{file_path}#L{line}")
}

fn github_issue_api_url(issue_id: &str) -> String {
    format!("https://api.github.com/repos/ZcashFoundation/zebra/issues/{issue_id}")
}

#[derive(Debug)]
struct PossibleIssueRef {
    file_path: String,
    line_number: usize,
    column: usize,
    id: String,
}

/// Process entry point for `search-issue-refs`
#[allow(clippy::print_stdout, clippy::print_stderr)]
#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    color_eyre::install()?;

    let file_paths = search_directory(&".".into())?;

    // Zebra's github issue numbers could be up to 4 digits
    let issue_regex =
        Regex::new(r"(https://github.com/ZcashFoundation/zebra/issues/|#)(\d{1,4})").unwrap();

    let mut possible_issue_refs: Vec<PossibleIssueRef> = vec![];

    for file_path in file_paths {
        let file = File::open(&file_path)?;
        let lines = io::BufReader::new(file).lines();

        for (line_idx, line) in lines.into_iter().enumerate() {
            let line = line?;

            possible_issue_refs.extend(issue_regex.captures_iter(&line).map(|captures| {
                let file_path = file_path
                    .to_str()
                    .expect("paths from read_dir should be valid unicode")
                    .to_string();

                let potential_issue_ref = captures.get(2).expect("matches should have 2 captures");
                let matching_text = potential_issue_ref.as_str();

                let id =
                    matching_text[matching_text.len().checked_sub(4).unwrap_or(1)..].to_string();

                PossibleIssueRef {
                    file_path,
                    line_number: line_idx + 1,
                    column: captures
                        .get(1)
                        .expect("matches should have 2 captures")
                        .start()
                        + 1,
                    id,
                }
            }))
        }
    }

    let num_possible_issue_refs = possible_issue_refs.len();

    println!(
        "\nFound {num_possible_issue_refs} possible issue refs, checking Github issue statuses..\n"
    );

    // check if issues are closed on Github

    let github_token = match env::vars().find(|(key, _)| key == GITHUB_TOKEN_ENV_KEY) {
        Some((_, github_token)) => github_token,
        _ => {
            println!(
                "Can't find {GITHUB_TOKEN_ENV_KEY} in env vars, printing all found possible issue refs,\
see https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/creating-a-personal-access-token \
to create a github token."
            );

            for PossibleIssueRef {
                file_path,
                line_number,
                column,
                id,
            } in possible_issue_refs
            {
                let github_url = github_issue_url(&id);
                let github_file_ref = github_remote_file_ref(&file_path, line_number);

                println!("\n--------------------------------------");
                println!("Found possible reference to closed issue #{id}: {file_path}:{line_number}:{column}");
                println!("{github_file_ref}");
                println!("{github_url}");
            }

            return Ok(());
        }
    };

    let mut headers = HeaderMap::new();
    let mut auth_value = HeaderValue::from_str(&format!("Bearer {github_token}"))?;
    let accept_value = HeaderValue::from_static("application/vnd.github+json");
    let github_api_version_value = HeaderValue::from_static("2022-11-28");
    let user_agent_value = HeaderValue::from_static("search-issue-refs");

    auth_value.set_sensitive(true);

    headers.insert(header::AUTHORIZATION, auth_value);
    headers.insert(header::ACCEPT, accept_value);
    headers.insert("X-GitHub-Api-Version", github_api_version_value);
    headers.insert(header::USER_AGENT, user_agent_value);

    let client = ClientBuilder::new().default_headers(headers).build()?;

    let mut github_api_requests = JoinSet::new();
    let mut num_possible_issue_refs = 0;

    for possible_issue_ref in possible_issue_refs {
        let request = client
            .get(github_issue_api_url(&possible_issue_ref.id))
            .send();
        github_api_requests.spawn(async move { (request.await, possible_issue_ref) });
    }

    while let Some(res) = github_api_requests.join_next().await {
        let Ok((
            res,
            PossibleIssueRef {
                file_path,
                line_number,
                column,
                id,
            },
        )) = res else {
            println!("warning: failed to join api request thread/task");
            continue;
        };

        let Ok(res) = res else {
            println!("warning: no response from github api about issue #{id}, {file_path}:{line_number}:{column}");
            continue;
        };

        let Ok(text) = res.text().await else {
            println!("warning: failed to get text from response about issue #{id}, {file_path}:{line_number}:{column}");
            continue;
        };

        let Ok(json): Result<serde_json::Value, _> = serde_json::from_str(&text) else {
            println!("warning: failed to get serde_json::Value from response for issue #{id}, {file_path}:{line_number}:{column}");
            continue;
        };

        if json["closed_at"] == serde_json::Value::Null {
            continue;
        };

        num_possible_issue_refs += 1;

        let github_url = github_issue_url(&id);
        let github_file_ref = github_remote_file_ref(&file_path, line_number);

        println!("\n--------------------------------------");
        println!("Found reference to closed issue #{id}: {file_path}:{line_number}:{column}");
        println!("{github_file_ref}");
        println!("{github_url}");
    }

    println!("\nFound {num_possible_issue_refs} references to closed issues.\n");

    Ok(())
}

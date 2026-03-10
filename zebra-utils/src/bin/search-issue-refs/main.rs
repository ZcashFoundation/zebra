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
    collections::HashMap,
    env,
    ffi::OsStr,
    fs::{self, File},
    io::{self, BufRead},
    path::PathBuf,
};

use color_eyre::eyre::{eyre, Result};
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
    let file_path = &crate_mod_path(file_path, line);
    format!("https://github.com/ZcashFoundation/zebra/blob/main/{file_path}")
}

fn github_permalink(sha: &str, file_path: &str, line: usize) -> String {
    let file_path = &crate_mod_path(file_path, line);
    format!("https://github.com/ZcashFoundation/zebra/blob/{sha}/{file_path}")
}

fn crate_mod_path(file_path: &str, line: usize) -> String {
    let file_path = &file_path[2..];
    format!("{file_path}#L{line}")
}

fn github_issue_api_url(issue_id: &str) -> String {
    format!("https://api.github.com/repos/ZcashFoundation/zebra/issues/{issue_id}")
}

fn github_ref_api_url(reference: &str) -> String {
    format!("https://api.github.com/repos/ZcashFoundation/zebra/git/ref/{reference}")
}

#[derive(Debug)]
struct PossibleIssueRef {
    file_path: String,
    line_number: usize,
    column: usize,
}

impl PossibleIssueRef {
    #[allow(clippy::print_stdout, clippy::print_stderr)]
    fn print_paths(issue_refs: &[PossibleIssueRef]) {
        for PossibleIssueRef {
            file_path,
            line_number,
            column,
        } in issue_refs
        {
            let file_ref = format!("{file_path}:{line_number}:{column}");
            let github_file_ref = github_remote_file_ref(file_path, *line_number);

            println!("{file_ref}\n{github_file_ref}\n");
        }
    }
}

type IssueId = String;

/// Process entry point for `search-issue-refs`
#[allow(clippy::print_stdout, clippy::print_stderr, clippy::unwrap_in_result)]
#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    color_eyre::install()?;

    let possible_issue_refs = {
        let file_paths = search_directory(&".".into())?;

        // Zebra's github issue numbers could be up to 4 digits
        let issue_regex =
            Regex::new(r"(https://github.com/ZcashFoundation/zebra/issues/|#)(\d{1,4})")?;

        let mut possible_issue_refs: HashMap<IssueId, Vec<PossibleIssueRef>> = HashMap::new();
        let mut num_possible_issue_refs = 0;

        for file_path in file_paths {
            let file = File::open(&file_path)?;
            let lines = io::BufReader::new(file).lines();

            for (line_idx, line) in lines.into_iter().enumerate() {
                let line = line?;
                let line_number = line_idx + 1;

                for captures in issue_regex.captures_iter(&line) {
                    let file_path = file_path
                        .to_str()
                        .ok_or_else(|| eyre!("paths from read_dir should be valid unicode"))?
                        .to_string();

                    let column = captures
                        .get(1)
                        .ok_or_else(|| eyre!("matches should have 2 captures"))?
                        .start()
                        + 1;

                    let potential_issue_ref = captures
                        .get(2)
                        .ok_or_else(|| eyre!("matches should have 2 captures"))?;
                    let matching_text = potential_issue_ref.as_str();

                    let id = matching_text[matching_text.len().checked_sub(4).unwrap_or(1)..]
                        .to_string();

                    let issue_entry = possible_issue_refs.entry(id).or_default();

                    if issue_entry.iter().all(|issue_ref| {
                        issue_ref.line_number != line_number || issue_ref.file_path != file_path
                    }) {
                        num_possible_issue_refs += 1;

                        issue_entry.push(PossibleIssueRef {
                            file_path,
                            line_number,
                            column,
                        });
                    }
                }
            }
        }

        let num_possible_issues = possible_issue_refs.len();

        println!(
            "\nFound {num_possible_issue_refs} possible references to {num_possible_issues} issues, checking statuses on Github..\n"
        );

        possible_issue_refs
    };

    // check if issues are closed on Github

    let Some((_, github_token)) = env::vars().find(|(key, _)| key == GITHUB_TOKEN_ENV_KEY) else {
        println!(
            "Can't find {GITHUB_TOKEN_ENV_KEY} in env vars, printing all found possible issue refs, \
see https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/creating-a-personal-access-token \
to create a github token."
        );

        for (
            id,
            PossibleIssueRef {
                file_path,
                line_number,
                column,
            },
        ) in possible_issue_refs
            .into_iter()
            .flat_map(|(issue_id, issue_refs)| {
                issue_refs
                    .into_iter()
                    .map(move |issue_ref| (issue_id.clone(), issue_ref))
            })
        {
            let github_url = github_issue_url(&id);
            let github_file_ref = github_remote_file_ref(&file_path, line_number);

            println!("\n--------------------------------------");
            println!("Found possible reference to closed issue #{id}: {file_path}:{line_number}:{column}");
            println!("{github_file_ref}");
            println!("{github_url}");
        }

        return Ok(());
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

    // get latest commit sha on main

    let latest_commit_json: serde_json::Value = serde_json::from_str::<serde_json::Value>(
        &client
            .get(github_ref_api_url("heads/main"))
            .send()
            .await?
            .text()
            .await?,
    )?;

    let latest_commit_sha = latest_commit_json["object"]["sha"]
        .as_str()
        .ok_or_else(|| eyre!("response.object.sha should be a string"))?;

    let mut github_api_requests = JoinSet::new();

    for (id, issue_refs) in possible_issue_refs {
        let request = client.get(github_issue_api_url(&id)).send();
        github_api_requests.spawn(async move { (request.await, id, issue_refs) });
    }

    // print out closed issues

    let mut num_closed_issue_refs = 0;
    let mut num_closed_issues = 0;

    while let Some(res) = github_api_requests.join_next().await {
        let Ok((res, id, issue_refs)) = res else {
            println!("warning: failed to join api request thread/task");
            continue;
        };

        let Ok(res) = res else {
            println!("warning: no response from github api about issue #{id}");
            PossibleIssueRef::print_paths(&issue_refs);
            continue;
        };

        let Ok(text) = res.text().await else {
            println!("warning: no response from github api about issue #{id}");
            PossibleIssueRef::print_paths(&issue_refs);
            continue;
        };

        let Ok(json): Result<serde_json::Value, _> = serde_json::from_str(&text) else {
            println!("warning: no response from github api about issue #{id}");
            PossibleIssueRef::print_paths(&issue_refs);
            continue;
        };

        if json["closed_at"] == serde_json::Value::Null {
            continue;
        };

        println!("\n--------------------------------------\n- #{id}\n");

        num_closed_issues += 1;

        for PossibleIssueRef {
            file_path,
            line_number,
            column: _,
        } in issue_refs
        {
            num_closed_issue_refs += 1;

            let github_permalink = github_permalink(latest_commit_sha, &file_path, line_number);

            println!("{github_permalink}");
        }
    }

    println!(
        "\nConfirmed {num_closed_issue_refs} references to {num_closed_issues} closed issues.\n"
    );

    Ok(())
}

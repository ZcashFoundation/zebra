//! Checks Miner Ballots and tallies Miner Polls, as specified in the draft ZIP "Miner Sentiment
//! Polling".
//!
//! For usage please refer to the program help: `miner-poll --help`

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand};
use color_eyre::eyre::{bail, ensure, eyre, Result, WrapErr};
use serde_json::{json, Value};

use zebra_chain::{
    block::{Block, Height, MAX_BLOCK_BYTES},
    common::default_cache_dir,
    parameters::Network,
    primitives::zcash_note_encryption::zero_ovk_memos,
    serialization::ZcashDeserializeInto,
    transaction::Transaction,
};
use zebra_utils::init_tracing;

#[cfg(test)]
mod tests;

/// The prefix of every Miner Ballot.
const BALLOT_PREFIX: &[u8] = b"zpoll:";

/// The maximum length of an answer string, so that every Miner Ballot fits in a 512-byte memo.
const MAX_ANSWER_STRING_LEN: usize = 489;

/// The number of confirmations of the block at the Voting End Height that a final tally needs.
const FINAL_CONFIRMATIONS: i64 = 100;

/// The maximum length of an RPC response body: a hex-encoded block of the maximum size, with room
/// for the JSON-RPC envelope. Longer responses are rejected before they exhaust memory.
const MAX_RESPONSE_LEN: usize = 2 * MAX_BLOCK_BYTES as usize + 1024;

/// A question of a Poll Definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Question {
    /// The number of options, from 1 to 35.
    options: u8,

    /// The maximum number of options a Miner Ballot can select, for a question that allows
    /// selecting several options; `None` for a question that allows selecting one option.
    max_selections: Option<u8>,
}

impl Question {
    /// Returns the length of the longest answer to this question.
    fn max_answer_len(self) -> usize {
        self.max_selections.map_or(1, |max| usize::from(max) + 2)
    }
}

/// The parts of a Poll Definition that determine which Miner Ballots are valid.
#[derive(Clone, Debug, clap::Args)]
struct Poll {
    /// The Election Identifier.
    #[arg(long, value_parser = parse_election)]
    election: String,

    /// Each question, in order: its number of options, e.g. `4`, or for a question that allows
    /// selecting several options, its number of options and maximum number of selections, e.g.
    /// `6:3`.
    #[arg(
        long = "options",
        value_name = "OPTIONS",
        required = true,
        value_delimiter = ',',
        value_parser = parse_question,
    )]
    questions: Vec<Question>,
}

impl Poll {
    /// Checks that every Miner Ballot for the poll fits in a 512-byte memo.
    fn check_size(&self) -> Result<()> {
        let max_len: usize = self.questions.iter().map(|q| q.max_answer_len()).sum();
        ensure!(
            max_len <= MAX_ANSWER_STRING_LEN,
            "the longest answer string has {max_len} characters, more than {MAX_ANSWER_STRING_LEN}"
        );

        Ok(())
    }
}

/// miner-poll arguments
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Encodes answers as a Miner Ballot for the Election, and prints it.
    Encode {
        #[command(flatten)]
        poll: Poll,

        /// The answer to each question, in order: the selected option, the selected options of a
        /// multiple-choice question joined by `+`, or `-` to leave the question unanswered, e.g.
        /// `2,3,1,1,-` or `1+3+5,2`.
        #[arg(
            long,
            required = true,
            allow_hyphen_values = true,
            value_delimiter = ',',
            value_parser = parse_answer,
        )]
        answers: Vec<Answer>,
    },

    /// Checks whether data counts as a Miner Ballot for the Election, and prints its answers.
    ///
    /// The data is searched like the Miner Data or a text memo of a coinbase transaction, so it
    /// can include other text, such as a pool tag.
    Check {
        #[command(flatten)]
        poll: Poll,

        /// The data to check, e.g. `zpoll:nu8:2311-`.
        data: String,
    },

    /// Tallies the Miner Poll from the blocks of a node, and prints its results as JSON.
    ///
    /// Before the block at the Voting End Height has 100 confirmations, the results are
    /// provisional: they count only the blocks mined so far, and set `final` to `false`.
    Tally {
        #[command(flatten)]
        poll: Poll,

        /// The network of the Poll Definition: `mainnet` or `testnet`. Use `regtest` to test a
        /// setup on a Regtest node with its default parameters.
        #[arg(long, value_parser = parse_network)]
        network: Network,

        /// The Voting Start Height.
        #[arg(long)]
        start: Height,

        /// The Voting End Height.
        #[arg(long)]
        end: Height,

        /// The Participation Threshold, as a percentage.
        #[arg(long, value_parser = parse_percentage)]
        threshold: f64,

        /// Address and port of the node's RPC server.
        #[arg(long, default_value = "127.0.0.1:8232")]
        addr: SocketAddr,

        /// Directory of the node's RPC authentication cookie, which is used if the file exists.
        /// Defaults to Zebra's cache directory if `--addr` is a loopback address; otherwise, no
        /// cookie is sent unless this is given. Requests are plain HTTP.
        #[arg(long)]
        cookie_dir: Option<PathBuf>,
    },
}

/// Parses an Election Identifier.
fn parse_election(id: &str) -> Result<String, String> {
    let valid = (1..=16).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');

    if valid {
        Ok(id.to_owned())
    } else {
        Err("must have 1 to 16 characters, each `a`-`z`, `0`-`9`, or `-`".to_owned())
    }
}

/// Parses a percentage.
fn parse_percentage(percentage: &str) -> Result<f64, String> {
    match percentage.parse() {
        Ok(percentage) if (0.0..=100.0).contains(&percentage) => Ok(percentage),
        _ => Err("must be a number from 0 to 100".to_owned()),
    }
}

/// Parses a network name.
fn parse_network(network: &str) -> Result<Network, String> {
    match network {
        "regtest" => Ok(Network::new_regtest(Default::default())),
        network => network
            .parse()
            .map_err(|_| "must be `mainnet`, `testnet`, or `regtest`".to_owned()),
    }
}

/// Parses a question: its number of options, followed for a multiple-choice question by `:` and
/// its maximum number of selections.
fn parse_question(question: &str) -> Result<Question, String> {
    let (options, max_selections) = match question.split_once(':') {
        None => (question, None),
        Some((options, max_selections)) => (options, Some(max_selections)),
    };
    let question = options.parse().ok().and_then(|options| {
        let max_selections = max_selections.map(str::parse).transpose().ok()?;
        let valid = (1..=35).contains(&options)
            && max_selections.is_none_or(|max| (2..=options).contains(&max));

        valid.then_some(Question {
            options,
            max_selections,
        })
    });

    question.ok_or_else(|| {
        "must be a number of options from 1 to 35, optionally followed by `:` and a maximum \
         number of selections from 2 to the number of options"
            .to_owned()
    })
}

/// Parses an answer: the selected options joined by `+`, or `-` for no answer.
fn parse_answer(answer: &str) -> Result<Answer, String> {
    if answer == "-" {
        return Ok(Answer::new());
    }

    answer
        .split('+')
        .map(|option| match option.parse() {
            Ok(option @ 1..=35) => Ok(option),
            _ => Err("must be options from 1 to 35 joined by `+`, or `-`".to_owned()),
        })
        .collect()
}

/// The options a Miner Ballot selects for a question, in ascending order; none if the Miner Ballot
/// leaves the question unanswered.
type Answer = Vec<u8>;

/// The answer to each question of a Miner Ballot.
type Answers = Vec<Answer>;

/// Returns the option encoded by an Option Character, or `None` if `byte` is not an Option
/// Character.
fn option(byte: u8) -> Option<u8> {
    match byte {
        b'1'..=b'9' => Some(byte - b'0'),
        b'A'..=b'Z' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Returns the Option Character that encodes `option`.
fn option_character(option: u8) -> char {
    match option {
        1..=9 => char::from(b'0' + option),
        // Options are at most 35, so this is `A` to `Z`.
        _ => char::from(b'A' + option - 10),
    }
}

/// Returns `true` if `byte` is an Answer Character: an Option Character, `-`, `(`, or `)`.
fn is_answer_character(byte: u8) -> bool {
    option(byte).is_some() || matches!(byte, b'-' | b'(' | b')')
}

/// Why answers don't form a valid Miner Ballot for a poll.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
enum InvalidBallot {
    #[error("{answers} answers for {questions} questions")]
    Length { answers: usize, questions: usize },

    #[error("no question is answered")]
    Unanswered,

    #[error("question {question} has only {options} options")]
    Option { question: usize, options: u8 },

    #[error("question {question} allows at most {max} selected options")]
    Selections { question: usize, max: u8 },

    #[error("question {question} selects options out of ascending order, or an option twice")]
    Order { question: usize },
}

/// Checks that `answers` form a valid Miner Ballot for the poll.
fn validate(answers: &Answers, poll: &Poll) -> Result<(), InvalidBallot> {
    if answers.len() != poll.questions.len() {
        return Err(InvalidBallot::Length {
            answers: answers.len(),
            questions: poll.questions.len(),
        });
    }
    if answers.iter().all(Vec::is_empty) {
        return Err(InvalidBallot::Unanswered);
    }
    for (question, (answer, definition)) in (1..).zip(answers.iter().zip(&poll.questions)) {
        let Question {
            options,
            max_selections,
        } = *definition;
        let max = max_selections.unwrap_or(1);
        if answer.len() > usize::from(max) {
            return Err(InvalidBallot::Selections { question, max });
        }
        if answer.iter().any(|&option| option > options) {
            return Err(InvalidBallot::Option { question, options });
        }
        if !answer.is_sorted_by(|a, b| a < b) {
            return Err(InvalidBallot::Order { question });
        }
    }

    Ok(())
}

/// Returns the Miner Ballot that encodes `answers` for the poll.
fn encode(answers: &Answers, poll: &Poll) -> Result<String, InvalidBallot> {
    validate(answers, poll)?;
    let mut ballot = format!("zpoll:{}:", poll.election);

    for (answer, question) in answers.iter().zip(&poll.questions) {
        let options = answer.iter().map(|&option| option_character(option));
        match (answer.is_empty(), question.max_selections) {
            (true, _) => ballot.push('-'),
            (false, None) => ballot.extend(options),
            (false, Some(_)) => {
                ballot.push('(');
                ballot.extend(options);
                ballot.push(')');
            }
        }
    }

    Ok(ballot)
}

/// Returns `answers` as JSON: for each question, `null` if it is unanswered, else the selected
/// option of a single-choice question, or the list of selected options of a multiple-choice
/// question.
fn answers_json(answers: &Answers, poll: &Poll) -> Value {
    answers
        .iter()
        .zip(&poll.questions)
        .map(
            |(answer, question)| match (answer.as_slice(), question.max_selections) {
                ([], _) => Value::Null,
                ([option], None) => json!(option),
                (options, _) => json!(options),
            },
        )
        .collect()
}

/// Parses an answer string into one answer per question of the poll: `-`, or an Option Character
/// for a single-choice question, or a parenthesized non-empty run of Option Characters for a
/// multiple-choice question. Returns `None` if the answer string doesn't have this form.
fn parse_answer_string(mut answer_string: &[u8], poll: &Poll) -> Option<Answers> {
    let mut answers = Answers::with_capacity(poll.questions.len());

    for question in &poll.questions {
        let (answer, rest) = match answer_string {
            [b'-', rest @ ..] => (Answer::new(), rest),
            [b'(', rest @ ..] if question.max_selections.is_some() => {
                let end = rest
                    .iter()
                    .position(|&b| b == b')')
                    .filter(|&end| end > 0)?;
                let answer = rest[..end]
                    .iter()
                    .map(|&b| option(b))
                    .collect::<Option<_>>()?;
                (answer, &rest[end + 1..])
            }
            [byte, rest @ ..] if question.max_selections.is_none() => (vec![option(*byte)?], rest),
            _ => return None,
        };
        answers.push(answer);
        answer_string = rest;
    }

    answer_string.is_empty().then_some(answers)
}

/// Returns the answers of the first Miner Ballot in `data` that is valid for the poll.
fn find_ballot(data: &[u8], poll: &Poll) -> Option<Answers> {
    (0..data.len()).find_map(|start| {
        let rest = data[start..]
            .strip_prefix(BALLOT_PREFIX)?
            .strip_prefix(poll.election.as_bytes())?
            .strip_prefix(b":")?;
        let len = rest
            .iter()
            .position(|&b| !is_answer_character(b))
            .unwrap_or(rest.len());
        let answers = parse_answer_string(&rest[..len], poll)?;

        validate(&answers, poll).is_ok().then_some(answers)
    })
}

/// Returns `true` if `memo` is a ZIP 302 text memo: its first byte is at most `0xF4`, and it is
/// valid UTF-8 without its trailing zero bytes.
fn is_text_memo(memo: &[u8; 512]) -> bool {
    let text_len = memo
        .iter()
        .rposition(|&b| b != 0)
        .map_or(0, |last| last + 1);

    memo[0] <= 0xF4 && std::str::from_utf8(&memo[..text_len]).is_ok()
}

/// Returns the answers of the Miner Ballot of the block at `height` whose coinbase transaction is
/// `coinbase`: the first valid one in the Miner Data, else in its text memos, in output order.
fn coinbase_ballot(
    coinbase: &Transaction,
    network: &Network,
    height: Height,
    poll: &Poll,
) -> Option<Answers> {
    let inputs = coinbase.inputs();
    let miner_data = inputs.first().and_then(|input| input.miner_data());

    miner_data
        .and_then(|data| find_ballot(data, poll))
        .or_else(|| {
            zero_ovk_memos(coinbase, network, height)
                .flatten()
                .filter(is_text_memo)
                .find_map(|memo| find_ballot(&memo, poll))
        })
}

/// The Miner Ballots of the blocks in a Voting Period.
#[derive(Debug)]
struct Tally {
    /// The number of blocks with a Miner Ballot.
    ballots: u32,

    /// The number of blocks whose Miner Ballot answers each question.
    answered: Vec<u32>,

    /// The number of blocks whose Miner Ballot selects each option of each question.
    votes: Vec<Vec<u32>>,
}

impl Tally {
    fn new(poll: &Poll) -> Self {
        Self {
            ballots: 0,
            answered: vec![0; poll.questions.len()],
            votes: poll
                .questions
                .iter()
                .map(|question| vec![0; question.options.into()])
                .collect(),
        }
    }

    fn add(&mut self, answers: &Answers) {
        self.ballots += 1;

        for ((answered, votes), answer) in
            self.answered.iter_mut().zip(&mut self.votes).zip(answers)
        {
            *answered += u32::from(!answer.is_empty());
            for option in answer {
                // Valid Miner Ballots only select options from 1 to the number of options.
                votes[usize::from(option - 1)] += 1;
            }
        }
    }

    /// Returns the results of a Voting Period with `blocks` blocks, with percentages relative to
    /// `blocks`.
    fn results(&self, blocks: u32, threshold: f64) -> Value {
        let percentage = |count: u32| 100.0 * f64::from(count) / f64::from(blocks);

        let questions: Vec<Value> = self
            .votes
            .iter()
            .zip(&self.answered)
            .zip(1..)
            .map(|((votes, &answered), question)| {
                let options: Vec<Value> = votes
                    .iter()
                    .zip(1..)
                    .map(|(&count, option): (&u32, u32)| {
                        json!({ "option": option, "votes": count, "percentage": percentage(count) })
                    })
                    .collect();

                json!({
                    "question": question,
                    "options": options,
                    "not_answered": blocks - answered,
                    "not_answered_percentage": percentage(blocks - answered),
                    "representative": percentage(answered) >= threshold,
                })
            })
            .collect();

        json!({
            "blocks": blocks,
            "blocks_with_ballot": self.ballots,
            "participation_percentage": percentage(self.ballots),
            "questions": questions,
        })
    }
}

/// A JSON-RPC client for a node.
struct Rpc {
    client: reqwest::Client,
    url: String,
    cookie: Option<String>,
}

impl Rpc {
    /// Returns a client for the node at `addr`, authenticated with the cookie in `cookie_dir` if
    /// the node wrote one there.
    fn new(addr: SocketAddr, cookie_dir: Option<&Path>) -> Self {
        // Zebra writes `__cookie__:<password>` to this file if cookie authentication is enabled.
        let cookie = cookie_dir
            .and_then(|dir| std::fs::read_to_string(dir.join(".cookie")).ok())
            .and_then(|cookie| Some(cookie.split_once(':')?.1.trim().to_owned()));

        Self {
            // The node is a socket address, so it is reached directly: a proxy from the
            // environment would receive the cookie, and can't reach a node on this machine.
            client: reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("the TLS backend initializes, as reqwest::Client::new() assumes"),
            url: format!("http://{addr}"),
            cookie,
        }
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let mut request = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json")
            .body(
                json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
                    .to_string(),
            );
        if let Some(cookie) = &self.cookie {
            request = request.basic_auth("__cookie__", Some(cookie));
        }

        let mut response = request
            .send()
            .await
            .wrap_err("can't connect to the node's RPC server")?;
        let status = response.status();
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                body.len() + chunk.len() <= MAX_RESPONSE_LEN,
                "{method}: the node's response is longer than {MAX_RESPONSE_LEN} bytes"
            );
            body.extend_from_slice(&chunk);
        }
        let mut response: Value = serde_json::from_slice(&body).wrap_err_with(|| {
            let body = String::from_utf8_lossy(&body);
            format!("{method}: unexpected HTTP {status} response: {body:?}")
        })?;

        if !response["error"].is_null() {
            bail!("{method}: {}", response["error"]);
        }

        Ok(response["result"].take())
    }
}

/// Returns the directory of the RPC cookie to send to the node at `addr`: `cookie_dir` if given,
/// else Zebra's cache directory if the node is on this machine.
///
/// The cookie grants full access to the RPC server of the node that wrote it, and requests are
/// plain HTTP, so the default cookie is never sent to another machine.
fn cookie_dir_for(addr: SocketAddr, cookie_dir: Option<PathBuf>) -> Option<PathBuf> {
    cookie_dir.or_else(|| addr.ip().is_loopback().then(default_cache_dir))
}

/// Tallies a Miner Poll and returns its results.
#[allow(clippy::print_stderr)]
async fn tally(
    poll: &Poll,
    network: &Network,
    start: Height,
    end: Height,
    threshold: f64,
    rpc: &Rpc,
) -> Result<Value> {
    ensure!(
        start <= end,
        "the Voting End Height is before the Voting Start Height"
    );

    // Testnet and Regtest nodes report the same chain name, but have different genesis blocks.
    let genesis = rpc.call("getblockhash", json!([0])).await?;
    ensure!(
        genesis.as_str() == Some(&network.genesis_hash().to_string()),
        "the node is not on {network}: its genesis block is {genesis}"
    );
    let info = rpc.call("getblockchaininfo", json!([])).await?;
    let tip = info["blocks"]
        .as_u64()
        .and_then(|tip| u32::try_from(tip).ok())
        .ok_or_else(|| eyre!("getblockchaininfo: unexpected blocks: {}", info["blocks"]))?;
    ensure!(
        tip >= start.0,
        "the Voting Period has not started: the node's tip is at height {tip}"
    );

    let last = tip.min(end.0);
    let mut tally = Tally::new(poll);
    let mut end_hash = None;
    let mut parent = None;

    for height in start.0..=last {
        let block = rpc.call("getblock", json!([height.to_string(), 0])).await?;
        let block: Block = hex::decode(block.as_str().ok_or_else(|| eyre!("getblock: no data"))?)?
            .zcash_deserialize_into()?;
        ensure!(
            block.coinbase_height() == Some(Height(height)),
            "the node returned a different block than requested"
        );
        // Blocks are fetched by height, so a reorg during the scan could mix blocks from two
        // chains.
        ensure!(
            parent.is_none_or(|parent| block.header.previous_block_hash == parent),
            "the node's best chain changed during the tally; run it again"
        );
        let hash = block.hash();
        parent = Some(hash);

        if let Some(answers) =
            coinbase_ballot(&block.transactions[0], network, Height(height), poll)
        {
            tally.add(&answers);
        }
        if height == end.0 {
            end_hash = Some(hash.to_string());
        }
        if (height - start.0) % 10_000 == 9_999 {
            eprintln!("tallied blocks up to height {height}");
        }
    }
    // Confirmations on the node's current best chain, which are negative if the tallied chain
    // is no longer part of it. They also decide finality, because the best chain is the one with
    // the most work, not necessarily the highest tip.
    let last_hash = parent.expect("the scan covers at least the block at the Voting Start Height");
    let header = rpc
        .call("getblockheader", json!([last_hash.to_string(), true]))
        .await?;
    let confirmations = header["confirmations"]
        .as_i64()
        .ok_or_else(|| eyre!("getblockheader: unexpected confirmations: {header}"))?;
    ensure!(
        confirmations > 0,
        "the node's best chain changed during the tally; run it again"
    );

    let mut results = json!({
        "election": poll.election,
        "network": network.to_string(),
        "voting_start_height": start.0,
        "voting_end_height": end.0,
        "voting_end_block_hash": end_hash,
        "participation_threshold_percentage": threshold,
        "final": last == end.0 && confirmations >= FINAL_CONFIRMATIONS,
        "tallied_through_height": last,
    });
    let blocks = end.0 - start.0 + 1;
    if let (Some(results), Value::Object(counts)) =
        (results.as_object_mut(), tally.results(blocks, threshold))
    {
        results.extend(counts);
    }

    Ok(results)
}

/// Process entry point for `miner-poll`
#[tokio::main]
#[allow(clippy::print_stdout)]
async fn main() -> Result<()> {
    init_tracing();
    color_eyre::install()?;

    let output = match Args::parse().command {
        Command::Encode { poll, answers } => {
            poll.check_size()?;
            encode(&answers, &poll)?
        }
        Command::Check { poll, data } => {
            poll.check_size()?;
            let answers = find_ballot(data.as_bytes(), &poll)
                .ok_or_else(|| eyre!("no valid Miner Ballot for this Election"))?;
            let answers = answers_json(&answers, &poll);
            serde_json::to_string_pretty(&json!({ "election": poll.election, "answers": answers }))?
        }
        Command::Tally {
            poll,
            network,
            start,
            end,
            threshold,
            addr,
            cookie_dir,
        } => {
            poll.check_size()?;
            let results = tally(
                &poll,
                &network,
                start,
                end,
                threshold,
                &Rpc::new(addr, cookie_dir_for(addr, cookie_dir).as_deref()),
            )
            .await?;
            serde_json::to_string_pretty(&results)?
        }
    };

    println!("{output}");

    Ok(())
}

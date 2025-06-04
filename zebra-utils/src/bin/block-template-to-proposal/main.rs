//! Transforms a JSON block template into a hex-encoded block proposal.
//!
//! Prints the parsed template and parsed proposal structures to stderr.
//!
//! For usage please refer to the program help: `block-template-to-proposal --help`

mod args;

use std::io::Read;

use color_eyre::eyre::Result;
use serde_json::Value;
use structopt::StructOpt;

use zebra_chain::{
    parameters::NetworkUpgrade,
    serialization::{DateTime32, ZcashSerialize},
};
use zebra_rpc::{
    client::constants::LONG_POLL_ID_LENGTH, client::types::TemplateResponse,
    proposal_block_from_template,
};
use zebra_utils::init_tracing;

/// The minimum number of characters in a valid `getblocktemplate JSON response.
///
/// The fields we use take up around ~800 bytes.
const MIN_TEMPLATE_BYTES: usize = 500;

/// Process entry point for `block-template-to-proposal`
#[allow(clippy::print_stdout, clippy::print_stderr)]
fn main() -> Result<()> {
    // initialise
    init_tracing();
    color_eyre::install()?;

    // get arguments from command-line or stdin
    let args = args::Args::from_args();

    let time_source = args.time_source;

    // Get template from command-line or standard input
    let template = args.template.unwrap_or_else(|| {
        let mut template = String::new();
        let bytes_read = std::io::stdin().read_to_string(&mut template).expect("missing JSON block template: must be supplied on command-line or standard input");

        if bytes_read < MIN_TEMPLATE_BYTES {
            panic!("JSON block template is too small: expected at least {MIN_TEMPLATE_BYTES} characters");
        }

        template
    });

    // parse string to generic json
    let mut template: Value = serde_json::from_str(&template)?;
    eprintln!(
        "{}",
        serde_json::to_string_pretty(&template).expect("re-serialization never fails")
    );

    let template_obj = template
        .as_object_mut()
        .expect("template must be a JSON object");

    // replace zcashd keys that are incompatible with Zebra

    // the longpollid key is in a node-specific format, but this tool doesn't use it,
    // so we can replace it with a dummy value
    template_obj["longpollid"] = "0".repeat(LONG_POLL_ID_LENGTH).into();

    // provide dummy keys that Zebra requires but zcashd does not always have

    // the transaction.*.required keys are not used by this tool,
    // so we can use any value here
    template_obj["coinbasetxn"]["required"] = true.into();
    for tx in template_obj["transactions"]
        .as_array_mut()
        .expect("transactions must be a JSON array")
    {
        tx["required"] = false.into();
    }

    // the maxtime field is used by this tool
    // if it is missing, substitute a valid value
    let current_time: DateTime32 = template_obj["curtime"]
        .to_string()
        .parse()
        .expect("curtime is always a valid DateTime32");

    template_obj.entry("maxtime").or_insert_with(|| {
        if time_source.uses_max_time() {
            eprintln!("maxtime field is missing, using curtime for maxtime: {current_time:?}");
        }

        current_time.timestamp().into()
    });

    // parse the modified json to template type
    let template: TemplateResponse = serde_json::from_value(template)?;

    // generate proposal according to arguments
    let proposal = proposal_block_from_template(&template, time_source, NetworkUpgrade::Nu5)?;
    eprintln!("{proposal:#?}");

    let proposal = proposal
        .zcash_serialize_to_vec()
        .expect("serialization to Vec never fails");

    println!("{}", hex::encode(proposal));

    Ok(())
}

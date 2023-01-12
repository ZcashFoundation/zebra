//! Transforms a JSON block template into a hex-encoded block proposal.
//!
//! Prints the parsed template and parsed proposal structures to stderr.
//!
//! For usage please refer to the program help: `block-template-to-proposal --help`

use std::io::Read;

use color_eyre::eyre::Result;
use structopt::StructOpt;

use zebra_chain::serialization::ZcashSerialize;
use zebra_rpc::methods::get_block_template_rpcs::{
    get_block_template::proposal_block_from_template, types::get_block_template::GetBlockTemplate,
};
use zebra_utils::init_tracing;

mod args;

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

    // parse json
    let template: GetBlockTemplate = serde_json::from_str(&template)?;
    eprintln!("{template:?}");

    // generate proposal according to arguments
    let proposal = proposal_block_from_template(template)?;
    eprintln!("{proposal:?}");

    let proposal = proposal
        .zcash_serialize_to_vec()
        .expect("serialization to Vec never fails");

    println!("{}", hex::encode(proposal));

    Ok(())
}

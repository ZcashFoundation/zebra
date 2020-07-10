use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct Args {
    /// Use the test network
    #[structopt(short, long)]
    pub testnet: bool,

    /// Passthrough args for `zcash-cli`
    #[structopt(last = true)]
    pub zcli_args: Vec<String>,
}

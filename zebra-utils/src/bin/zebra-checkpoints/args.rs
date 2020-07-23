use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct Args {
    /// Path to zcash-cli command
    #[structopt(short, long)]
    pub cli: String,

    /// Passthrough args for `zcash-cli`
    #[structopt(last = true)]
    pub zcli_args: Vec<String>,
}

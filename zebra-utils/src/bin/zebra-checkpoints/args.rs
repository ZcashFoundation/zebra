use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct Args {
    /// Path to zcash-cli command
    #[structopt(short, long)]
    pub cli: String,

    /// Use the mainnet network
    #[structopt(short, long)]
    pub mainnet: bool,

    /// Use the regtest network
    #[structopt(short, long)]
    pub regtest: bool,

    /// Use the test network
    #[structopt(short, long)]
    pub testnet: bool,
}

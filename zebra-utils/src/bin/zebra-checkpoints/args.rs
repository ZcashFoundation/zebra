use structopt::StructOpt;
use structopt::clap::arg_enum;

arg_enum!{
    #[derive(PartialEq, Debug)]
    pub enum Network {
        Mainnet,
        Testnet,
        Regtest,
    }
}

#[derive(Debug, StructOpt)]
pub struct Args {
    /// Path to zcash-cli command
    #[structopt(short, long)]
    pub cli: String,

    /// Network to use
    #[structopt(default_value = "mainnet", short, long)]
    pub network: Network,
}

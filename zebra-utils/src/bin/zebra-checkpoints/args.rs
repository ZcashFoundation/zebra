//! zebra-checkpoints arguments
//!
//! For usage please refer to the program help: `zebra-checkpoints --help`

use std::{net::SocketAddr, str::FromStr};

use structopt::StructOpt;
use thiserror::Error;

use zebra_chain::block::Height;

/// The backend type the zebra-checkpoints utility will use to get data from.
///
/// This changes which RPCs the tool calls, and which fields it expects them to have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Expect a Zebra-style backend with limited RPCs and fields.
    ///
    /// Calls these specific RPCs:
    /// - `getblock` with `verbose=0`, manually calculating `hash`, `height`, and `size`
    /// - `getblockchaininfo`, expecting a `blocks` field
    ///
    /// Supports both `zebrad` and `zcashd` nodes.
    Zebrad,

    /// Expect a `zcashd`-style backend with all available RPCs and fields.
    ///
    /// Calls these specific RPCs:
    /// - `getblock` with `verbose=1`, expecting `hash`, `height`, and `size` fields
    /// - `getblockchaininfo`, expecting a `blocks` field
    ///
    /// Currently only supported with `zcashd`.
    Zcashd,
}

impl FromStr for Backend {
    type Err = InvalidBackendError;

    fn from_str(string: &str) -> Result<Self, Self::Err> {
        match string.to_lowercase().as_str() {
            "zebrad" => Ok(Backend::Zebrad),
            "zcashd" => Ok(Backend::Zcashd),
            _ => Err(InvalidBackendError(string.to_owned())),
        }
    }
}

/// An error indicating that the supplied string is not a valid [`Backend`] name.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("Invalid backend: {0}")]
pub struct InvalidBackendError(String);

/// The transport used by the zebra-checkpoints utility to connect to the [`Backend`].
///
/// This changes how the tool makes RPC requests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transport {
    /// Launch the `zcash-cli` command in a subprocess, and read its output.
    ///
    /// The RPC name and parameters are sent as command-line arguments.
    /// Responses are read from the command's standard output.
    ///
    /// Requires the `zcash-cli` command, which is part of `zcashd`'s tools.
    /// Supports both `zebrad` and `zcashd` nodes.
    Cli,

    /// Connect directly to the node using TCP, and use the JSON-RPC protocol.
    ///
    /// Uses JSON-RPC over HTTP for sending the RPC name and parameters, and
    /// receiving responses.
    ///
    /// Always supports the `zebrad` node.
    /// Only supports `zcashd` nodes using a JSON-RPC TCP port with no authentication.
    Direct,
}

impl FromStr for Transport {
    type Err = InvalidTransportError;

    fn from_str(string: &str) -> Result<Self, Self::Err> {
        match string.to_lowercase().as_str() {
            "cli" | "zcash-cli" | "zcashcli" | "zcli" | "z-cli" => Ok(Transport::Cli),
            "direct" => Ok(Transport::Direct),
            _ => Err(InvalidTransportError(string.to_owned())),
        }
    }
}

/// An error indicating that the supplied string is not a valid [`Transport`] name.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("Invalid transport: {0}")]
pub struct InvalidTransportError(String);

/// zebra-checkpoints arguments
#[derive(Clone, Debug, Eq, PartialEq, StructOpt)]
pub struct Args {
    /// Backend type: the node we're connecting to.
    #[structopt(default_value = "zebrad", short, long)]
    pub backend: Backend,

    /// Transport type: the way we connect.
    #[structopt(default_value = "cli", short, long)]
    pub transport: Transport,

    /// Path or name of zcash-cli command.
    /// Only used if the transport is [`Cli`](Transport::Cli).
    #[structopt(default_value = "zcash-cli", short, long)]
    pub cli: String,

    /// Address and port for RPC connections.
    /// Used for all transports.
    #[structopt(short, long)]
    pub addr: Option<SocketAddr>,

    /// Start looking for checkpoints after this height.
    /// If there is no last checkpoint, we start looking at the Genesis block (height 0).
    #[structopt(short, long)]
    pub last_checkpoint: Option<Height>,

    /// Passthrough args for `zcash-cli`.
    /// Only used if the transport is [`Cli`](Transport::Cli).
    #[structopt(last = true)]
    pub zcli_args: Vec<String>,
}

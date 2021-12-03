//! `getnewaddress` subcommand

use std::convert::TryInto;

use abscissa_core::{config, Command, FrameworkError, Options, Runnable};
use qrcode::{render::unicode, QrCode};
use zcash_address::{self, unified, ToAddress};

use zebra_chain::{orchard, sapling};

use crate::config::ZebraCliConfig;

/// `getnewaddress` subcommand
///
/// The `Options` proc macro generates an option parser based on the struct
/// definition, and is defined in the `gumdrop` crate. See their documentation
/// for a more comprehensive example:
///
/// <https://docs.rs/gumdrop/>
#[derive(Command, Debug, Options)]
pub struct GetNewAddressCmd {
    // Example `--foobar` (with short `-f` argument)
// #[options(short = "f", help = "foobar path"]
// foobar: Option<PathBuf>

// Example `--baz` argument with no short version
// #[options(no_short, help = "baz path")]
// baz: Options<PathBuf>

// "free" arguments don't have an associated flag
// #[options(free)]
// free_args: Vec<String>,
}

impl config::Override<ZebraCliConfig> for GetNewAddressCmd {
    // Process the given command line options, overriding settings from
    // a configuration file using explicit flags taken from command-line
    // arguments.
    fn override_config(&self, config: ZebraCliConfig) -> Result<ZebraCliConfig, FrameworkError> {
        Ok(config)
    }
}

impl Runnable for GetNewAddressCmd {
    /// Start the application.
    fn run(&self) {
        let network = zebra_chain::parameters::Network::Mainnet;

        // Sapling

        let spending_key = sapling::keys::SpendingKey::new(&mut rand::rngs::OsRng);

        let spend_authorizing_key = sapling::keys::SpendAuthorizingKey::from(spending_key);
        let proof_authorizing_key = sapling::keys::ProofAuthorizingKey::from(spending_key);
        let _outgoing_viewing_key = sapling::keys::OutgoingViewingKey::from(spending_key);

        let authorizing_key = sapling::keys::AuthorizingKey::from(spend_authorizing_key);
        let nullifier_deriving_key =
            sapling::keys::NullifierDerivingKey::from(proof_authorizing_key);
        let incoming_viewing_key =
            sapling::keys::IncomingViewingKey::from((authorizing_key, nullifier_deriving_key));

        let diversifier = sapling::keys::Diversifier::from(spending_key);
        let transmission_key =
            sapling::keys::TransmissionKey::from((incoming_viewing_key, diversifier));

        let sapling_address = sapling::Address::new(network, diversifier, transmission_key);

        // Orchard

        let spending_key = orchard::keys::SpendingKey::new(&mut rand::rngs::OsRng, network);

        let spend_authorizing_key = orchard::keys::SpendAuthorizingKey::from(spending_key);
        let _spend_validating_key = orchard::keys::SpendValidatingKey::from(spend_authorizing_key);
        let _nullifier_deriving_key = orchard::keys::NullifierDerivingKey::from(spending_key);
        let _ivk_commit_randomness = orchard::keys::IvkCommitRandomness::from(spending_key);

        let full_viewing_key = orchard::keys::FullViewingKey::from(spending_key);

        let diversifier_key = orchard::keys::DiversifierKey::from(full_viewing_key);
        let incoming_viewing_key = orchard::keys::IncomingViewingKey::from(full_viewing_key);
        let _outgoing_viewing_key = orchard::keys::OutgoingViewingKey::from(full_viewing_key);
        let diversifier = orchard::keys::Diversifier::from(diversifier_key);
        let transmission_key =
            orchard::keys::TransmissionKey::from((incoming_viewing_key, diversifier));

        let orchard_address = orchard::Address::new(diversifier, transmission_key);

        // Encode as a unified address
        let receivers = vec![
            unified::Receiver::Sapling(sapling_address.into()),
            unified::Receiver::Orchard(orchard_address.into()),
        ];

        let zcash_addr = zcash_address::ZcashAddress::from_unified(
            zcash_address::Network::Main,
            receivers.try_into().expect("a valid unified::Address"),
        );

        // Print to stdout as a QR code

        let code = QrCode::new(zcash_addr.to_string()).unwrap();

        let image = code
            .render::<unicode::Dense1x2>()
            .dark_color(unicode::Dense1x2::Light)
            .light_color(unicode::Dense1x2::Dark)
            .build();

        println!("\nNew Unified Zcash Address:");
        println!("\n{}\n", zcash_addr);
        println!("\n{}\n", image);
    }
}

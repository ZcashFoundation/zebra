//! Tests for parsing zebrad commands

use clap::Parser;

use crate::commands::ZebradCmd;

use super::EntryPoint;

#[test]
fn args_with_subcommand_pass_through() {
    let test_cases = [
        (false, true, false, vec!["zebrad"]),
        (false, true, true, vec!["zebrad", "-v"]),
        (false, true, true, vec!["zebrad", "--verbose"]),
        (true, false, false, vec!["zebrad", "-h"]),
        (true, false, false, vec!["zebrad", "--help"]),
        (false, true, false, vec!["zebrad", "start"]),
        (false, true, true, vec!["zebrad", "-v", "start"]),
        (false, true, false, vec!["zebrad", "--filters", "warn"]),
        (true, false, false, vec!["zebrad", "warn"]),
        (false, true, false, vec!["zebrad", "start", "warn"]),
        (true, false, false, vec!["zebrad", "help", "warn"]),
    ];

    for (should_exit, should_be_start, should_be_verbose, args) in test_cases {
        let args = EntryPoint::process_cli_args(args.iter().map(Into::into).collect());

        if should_exit {
            args.expect_err("parsing invalid args or 'help'/'--help' should return an error");
            continue;
        }

        let args: Vec<std::ffi::OsString> = args.expect("args should parse into EntryPoint");

        let args =
            EntryPoint::try_parse_from(args).expect("hardcoded args should parse successfully");

        assert!(args.config.is_none(), "args.config should be none");
        assert!(args.cmd.is_some(), "args.cmd should not be none");
        assert_eq!(
            args.verbose, should_be_verbose,
            "process_cli_args should preserve top-level args"
        );

        assert_eq!(matches!(args.cmd(), ZebradCmd::Start(_)), should_be_start,);
    }
}

/// Startup policy, local advertisements, and RPC service reporting agree for every pruning state.
#[tokio::test(flavor = "current_thread")]
async fn pruned_network_services_preserve_independent_capabilities() {
    use std::sync::{Arc, Mutex};
    use tower::buffer::Buffer;
    use zebra_chain::{block::Height, chain_sync_status::MockSyncStatus, chain_tip::NoChainTip};
    use zebra_network::{types::PeerServices, AddressBook, AddressBookPeers};
    use zebra_node_services::BoxError;
    use zebra_rpc::methods::{RpcImpl, RpcServer};
    use zebra_state::HistoryCapabilities;
    use zebra_test::mock_service::MockService;

    let _init_guard = zebra_test::init();

    for independent_services in [
        PeerServices::empty(),
        PeerServices::from_bits_retain(1 << 63),
    ] {
        let mut config = crate::config::ZebradConfig::default();
        config.network.local_services |= independent_services;

        for (case, retention, lowest_retained_height, expected) in [
            ("archive", None, Height(0), PeerServices::NODE_NETWORK),
            (
                "pruned history",
                None,
                Height(10_000),
                PeerServices::empty(),
            ),
            (
                "pruning configured before deletion",
                Some(10_000),
                Height(0),
                PeerServices::empty(),
            ),
            (
                "pruning configured after deletion",
                Some(10_000),
                Height(10_000),
                PeerServices::empty(),
            ),
        ] {
            config.state.prune_blocks_older_than = retention;
            let services = super::StartCmd::network_services(
                &config,
                HistoryCapabilities {
                    lowest_retained_height,
                },
            )
            .unwrap();
            let expected = expected | independent_services;
            assert_eq!(services, expected, "{case}");

            let network = config.network.network.clone();
            let address_book = AddressBook::new(
                ([127, 0, 0, 1], network.default_port()).into(),
                services,
                &network,
                zebra_network::constants::DEFAULT_MAX_CONNS_PER_IP,
                tracing::Span::none(),
            );
            let advertised = address_book
                .local_listener_meta_addr(chrono::Utc::now())
                .services()
                .expect("local listener advertisements include their services");
            assert_eq!(advertised, expected, "{case}");
            assert_eq!(address_book.local_services(), advertised, "{case}");

            let (_log_tx, log_rx) = tokio::sync::watch::channel(None);
            let (rpc, rpc_tx_queue) = RpcImpl::new(
                network,
                Default::default(),
                Default::default(),
                "0.0.1",
                "RPC service reporting test",
                Buffer::new(MockService::build().for_unit_tests::<_, _, BoxError>(), 1),
                Buffer::new(MockService::build().for_unit_tests::<_, _, BoxError>(), 1),
                Buffer::new(MockService::build().for_unit_tests::<_, _, BoxError>(), 1),
                MockService::build().for_unit_tests(),
                MockSyncStatus::default(),
                NoChainTip,
                Arc::new(Mutex::new(address_book)),
                log_rx,
                None,
            );
            let response =
                tokio::time::timeout(std::time::Duration::from_secs(5), rpc.get_network_info())
                    .await
                    .expect("the test owns the uncontended address book")
                    .expect("network info reads only the address book");
            assert_eq!(
                response.local_services,
                format!("{advertised:016x}"),
                "{case}"
            );
            rpc_tx_queue.abort();
        }
    }
}

#[test]
fn pruned_history_rejects_both_sidecar_modes() {
    use zebra_chain::block::Height;
    use zebra_state::HistoryCapabilities;

    let mut config = crate::config::ZebradConfig::default();
    config.zcashd_compat.enabled = true;
    for managed in [false, true] {
        config.zcashd_compat.manage_zcashd = managed;
        config.state.prune_blocks_older_than = None;
        super::StartCmd::network_services(&config, HistoryCapabilities::default())
            .expect("archive nodes can serve sidecars");
        for (retention, lowest_retained_height) in [
            (None, Height(10_000)),
            (Some(10_000), Height(0)),
            (Some(10_000), Height(10_000)),
        ] {
            config.state.prune_blocks_older_than = retention;
            let error = super::StartCmd::network_services(
                &config,
                HistoryCapabilities {
                    lowest_retained_height,
                },
            )
            .expect_err("sidecars require complete block history and pruning disabled");
            assert!(error
                .to_string()
                .contains("zcashd-compat requires an archive node"));
        }
    }
}

#[test]
fn pruning_config_defaults_and_round_trips() {
    let config: crate::config::ZebradConfig = toml::from_str("").unwrap();
    assert_eq!(config.state.prune_blocks_older_than, None);

    for retention in [None, Some(10_000), Some(u32::MAX)] {
        let mut config = config.clone();
        config.state.prune_blocks_older_than = retention;
        let value = toml::Value::try_from(&config).unwrap();
        assert_eq!(
            value["state"].get("prune_blocks_older_than").is_some(),
            retention.is_some(),
        );
        let encoded = toml::to_string(&value).unwrap();
        let decoded: crate::config::ZebradConfig = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded, config);
    }

    let config: crate::config::ZebradConfig =
        toml::from_str("[state]\nprune_blocks_older_than = 10000\n").unwrap();
    assert_eq!(config.state.prune_blocks_older_than, Some(10_000));
}

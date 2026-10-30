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

#[tokio::test]
async fn halo2_key_preload_completion_does_not_exit_zebra() {
    use futures::{future::FusedFuture, FutureExt};

    let mut handle = tokio::task::spawn_blocking(|| ());
    let completed = (&mut handle).fuse();
    futures::pin_mut!(completed);
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), &mut completed)
        .await
        .expect("startup task must finish");

    assert!(!super::start::StartCmd::halo2_key_preload_should_exit(
        result
    ));
    assert!(completed.is_terminated());
    assert!(completed.now_or_never().is_none());
}

#[tokio::test]
#[should_panic(expected = "Halo2 verifying key construction must succeed")]
async fn halo2_key_preload_panic_exits_zebra() {
    let handle = tokio::task::spawn_blocking(|| panic!("simulated key construction panic"));
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), handle)
        .await
        .expect("startup task must finish");
    super::start::StartCmd::halo2_key_preload_should_exit(result);
}

#[test]
fn startup_rejects_pruning_sidecars_before_state_init() {
    use futures::FutureExt;

    for managed in [false, true] {
        for retention in [0, 10_000, u32::MAX] {
            let dir = tempfile::tempdir().unwrap();
            let state_path = dir.path().join("state");
            let mut config = crate::config::ZebradConfig::default();
            config.state.cache_dir = state_path.clone();
            config.state.prune_blocks_older_than = Some(retention);
            config.zcashd_compat.enabled = true;
            config.zcashd_compat.manage_zcashd = managed;

            // No runtime: reaching async state initialization fails this test.
            let error = super::StartCmd::default()
                .start(std::sync::Arc::new(config))
                .now_or_never()
                .expect("configuration errors must precede async initialization")
                .expect_err("sidecars require pruning disabled");
            assert!(!state_path.exists(), "startup must not open writable state");
            assert!(error
                .to_string()
                .contains("zcashd-compat requires an archive node"));
        }
    }
}

#[test]
fn pruned_network_services_preserve_independent_capabilities() {
    use zebra_chain::block::Height;
    use zebra_network::types::PeerServices;
    use zebra_state::HistoryCapabilities;

    let mut config = crate::config::ZebradConfig::default();
    config.network.local_services |= PeerServices::NODE_EARLY_BLOCK_RELAY;
    let archive_services = config.network.local_services;
    assert_eq!(
        super::StartCmd::network_services(&config, HistoryCapabilities::default()).unwrap(),
        archive_services,
    );
    for (retention, lowest_retained_height) in [
        (None, Height(10_000)),
        (Some(10_000), Height(0)),
        (Some(10_000), Height(10_000)),
    ] {
        config.state.prune_blocks_older_than = retention;
        assert_eq!(
            super::StartCmd::network_services(
                &config,
                HistoryCapabilities {
                    lowest_retained_height,
                },
            )
            .unwrap(),
            archive_services & !PeerServices::NODE_NETWORK,
        );
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

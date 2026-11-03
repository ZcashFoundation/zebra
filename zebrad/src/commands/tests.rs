//! Tests for parsing zebrad commands

use abscissa_core::Configurable;
use clap::Parser;
use zebra_state::MIN_PRUNING_RETENTION;

use crate::{commands::ZebradCmd, config::ZebradConfig};

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

#[test]
fn start_filters_override_config() {
    let test_cases = [
        (vec!["zebrad", "--filters", "warn"], "warn"),
        (vec!["zebrad", "--filters", "warn", "start"], "warn"),
        (
            vec![
                "zebrad",
                "--filters",
                "warn",
                "start",
                "info",
                "zebra_network=debug",
            ],
            "info,zebra_network=debug",
        ),
    ];

    for (args, expected_filter) in test_cases {
        let processed_args = EntryPoint::process_cli_args(args.iter().map(Into::into).collect())
            .expect("hardcoded args should process successfully");
        let command = EntryPoint::try_parse_from(processed_args)
            .expect("processed args should parse successfully");
        let mut config = ZebradConfig::default();
        config.tracing.filter = Some("error".to_owned());
        let config = command
            .process_config(config)
            .expect("command filters should override the configured filter");

        assert_eq!(
            config.tracing.filter.as_deref(),
            Some(expected_filter),
            "{args:?}"
        );
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
        for retention in [0, MIN_PRUNING_RETENTION, u32::MAX] {
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
fn pruned_network_services_clear_archive_capability() {
    use zebra_chain::block::Height;
    use zebra_network::types::PeerServices;
    use zebra_state::HistoryCapabilities;

    let mut config = crate::config::ZebradConfig::default();
    assert_eq!(
        super::StartCmd::network_services(&config, HistoryCapabilities::default()).unwrap(),
        PeerServices::NODE_NETWORK,
    );
    for (retention, lowest_retained_height, limited) in [
        (None, Height(MIN_PRUNING_RETENTION), false),
        (Some(MIN_PRUNING_RETENTION), Height(0), true),
        (
            Some(MIN_PRUNING_RETENTION),
            Height(MIN_PRUNING_RETENTION),
            true,
        ),
        // Fewer retained blocks than limited service promises, as on Regtest.
        (Some(1_001), Height(1_001), false),
    ] {
        config.state.prune_blocks_older_than = retention;
        let mut expected = PeerServices::empty();
        expected.set(PeerServices::NODE_NETWORK_LIMITED, limited);
        assert_eq!(
            super::StartCmd::network_services(
                &config,
                HistoryCapabilities {
                    lowest_retained_height,
                },
            )
            .unwrap(),
            expected,
            "retention {retention:?}",
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
            (None, Height(MIN_PRUNING_RETENTION)),
            (Some(MIN_PRUNING_RETENTION), Height(0)),
            (Some(MIN_PRUNING_RETENTION), Height(MIN_PRUNING_RETENTION)),
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

    for retention in [None, Some(MIN_PRUNING_RETENTION), Some(u32::MAX)] {
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
    assert_eq!(
        config.state.prune_blocks_older_than,
        Some(MIN_PRUNING_RETENTION)
    );
}

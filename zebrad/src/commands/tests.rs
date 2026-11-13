//! Tests for parsing zebrad commands

use abscissa_core::Configurable;
use clap::Parser;

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

//! Tests for parsing zebrad commands

use clap::Parser;

use crate::commands::ZebradCmd;

use super::EntryPoint;

#[test]
fn args_with_subcommand_pass_through() {
    let test_cases = [
        (true, false, vec!["zebrad"]),
        (true, true, vec!["zebrad", "-v"]),
        (true, true, vec!["zebrad", "--verbose"]),
        (false, false, vec!["zebrad", "-h"]),
        (false, false, vec!["zebrad", "--help"]),
        (true, false, vec!["zebrad", "start"]),
        (true, true, vec!["zebrad", "-v", "start"]),
        (true, false, vec!["zebrad", "warn"]),
        (true, false, vec!["zebrad", "start", "warn"]),
        (false, false, vec!["zebrad", "help", "warn"]),
    ];

    for (should_be_start, should_be_verbose, args) in test_cases {
        let args = EntryPoint::process_cli_args(args.iter().map(Into::into).collect());
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

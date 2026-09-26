//! The command tree itself: which subcommands exist, that each one's arguments
//! parse, and that the failures which carry a reserved exit code carry it.
//!
//! This is the test that stops a rename from being silent. `roost push` shells
//! out to `roost status`, `smoke/terminal/stack.ts` launches `roost worker`, and
//! a deploy's journal addresses `__keeper-contract` by string; a subcommand that
//! disappears or changes its arguments breaks all three without any Rust test
//! failing, so the surface is asserted here by name.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use clap::{CommandFactory, Parser};

use roost_cli::doctor::window::{DEFAULT_WINDOW_LABEL, parse_window};
use roost_cli::{Cli, Command};

/// Every subcommand the crate's dispatcher answers, in the order `--help`
/// prints them. The list is the contract `docs/phase6-cli-contract.md`
/// documents; a command added here must be documented there in the same change.
const SUBCOMMANDS: [&str; 12] = [
    "coord",
    "worker",
    "keeper",
    "status",
    "doctor",
    "version",
    "logs",
    "state",
    "reset",
    "skill",
    "test",
    "__keeper-contract",
];

#[test]
fn every_documented_subcommand_parses_with_no_arguments() {
    for name in SUBCOMMANDS {
        // The two commands with a required argument are exercised with the
        // argument `--help` says they need, because "it parses" is a claim
        // about the command, not about its argument being optional.
        let argv: Vec<String> = match name {
            "keeper" => vec![
                "roost".to_string(),
                name.to_string(),
                "/tmp/mux-keeper.sock".to_string(),
            ],
            "logs" => vec!["roost".to_string(), name.to_string(), "coord".to_string()],
            _ => vec!["roost".to_string(), name.to_string()],
        };
        let cli = Cli::try_parse_from(&argv)
            .unwrap_or_else(|error| panic!("{name} did not parse: {error}"));
        assert_eq!(cli.command.name(), name);
    }
}

#[test]
fn an_unknown_subcommand_is_refused_rather_than_guessed() {
    assert!(Cli::try_parse_from(["roost", "deploy"]).is_err());
    // The v2 spelling is gone on purpose: a deploy that silently became
    // something else would be worse than one that says it does not exist.
    assert!(Cli::try_parse_from(["roost", "cutover"]).is_err());
}

#[test]
fn a_bare_invocation_with_no_subcommand_is_a_usage_error() {
    assert!(Cli::try_parse_from(["roost"]).is_err());
}

#[test]
fn status_takes_an_endpoint_override_and_defaults_to_nothing() {
    let cli = Cli::try_parse_from(["roost", "status"]).unwrap();
    let Command::Status(args) = cli.command else {
        panic!("expected status");
    };
    assert_eq!(args.endpoint, None);

    let cli = Cli::try_parse_from(["roost", "status", "--endpoint", "https://x.test"]).unwrap();
    let Command::Status(args) = cli.command else {
        panic!("expected status");
    };
    assert_eq!(args.endpoint.as_deref(), Some("https://x.test"));
}

#[test]
fn doctor_defaults_to_a_day_and_takes_a_session() {
    let cli = Cli::try_parse_from(["roost", "doctor"]).unwrap();
    let Command::Doctor(args) = cli.command else {
        panic!("expected doctor");
    };
    assert_eq!(args.since, DEFAULT_WINDOW_LABEL);
    assert_eq!(args.session, None);

    let cli =
        Cli::try_parse_from(["roost", "doctor", "--since", "90m", "--session", "abc123"]).unwrap();
    let Command::Doctor(args) = cli.command else {
        panic!("expected doctor");
    };
    assert_eq!(args.since, "90m");
    assert_eq!(args.session.as_deref(), Some("abc123"));
}

#[test]
fn a_malformed_window_exits_two_and_not_one() {
    // clap accepts any string for `--since`; the window is what refuses, and it
    // refuses with 2 — not 1 — so a cron wrapper can tell "the operator mistyped
    // the argument" from "the window is alarming". The refusal happens before a
    // single log file is opened, which is why it lives in the window parser and
    // not in the digest.
    let cli = Cli::try_parse_from(["roost", "doctor", "--since", "24x"]).unwrap();
    let Command::Doctor(args) = cli.command else {
        panic!("expected doctor");
    };
    assert_eq!(args.since, "24x");
    assert_eq!(parse_window(&args.since).unwrap_err().code, 2);
}

#[test]
fn logs_takes_one_of_two_apps_and_a_tail_count() {
    for app in ["coord", "worker"] {
        let cli = Cli::try_parse_from(["roost", "logs", app, "--tail", "5"]).unwrap();
        let Command::Logs(args) = cli.command else {
            panic!("expected logs");
        };
        assert_eq!(args.tail, 5);
    }
    assert!(Cli::try_parse_from(["roost", "logs", "keeper"]).is_err());
}

#[test]
fn version_build_is_a_flag_and_not_a_positional() {
    assert!(Cli::try_parse_from(["roost", "version", "--build"]).is_ok());
    // v2 accepted `roost version --build` and refused a stray argument; keeping
    // the flag explicit keeps `roost version extra` a usage error.
    assert!(Cli::try_parse_from(["roost", "version", "extra"]).is_err());
}

#[test]
fn test_profiles_are_the_documented_set() {
    for profile in ["lint", "unit", "terminal", "upgrade", "live-api", "all"] {
        assert!(
            Cli::try_parse_from(["roost", "test", profile]).is_ok(),
            "{profile} did not parse"
        );
    }
    // `worker` was a v2 profile for isolating the JavaScript worker suite, and
    // there is no JavaScript worker suite in v3 to isolate.
    assert!(Cli::try_parse_from(["roost", "test", "worker"]).is_err());
}

#[test]
fn the_keeper_contract_probe_is_hidden_but_still_addressable() {
    // Hidden from `--help` on purpose — it exists for the deploy and upgrade
    // paths, not for an operator — and addressable by name, because those paths
    // address it as a string.
    let cli = Cli::try_parse_from(["roost", "__keeper-contract"]).unwrap();
    assert_eq!(cli.command.name(), "__keeper-contract");
    assert!(
        Cli::command()
            .get_subcommands()
            .any(|sub| sub.get_name() == "__keeper-contract")
    );
}

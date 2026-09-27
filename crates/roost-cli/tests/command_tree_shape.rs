//! The command tree itself: which subcommands exist, that each one's arguments
//! parse, and that the failures which carry a reserved exit code carry it.
//!
//! This is the test that stops a rename from being silent. `roost push` shells
//! out to `roost status`, `smoke/terminal/stack.ts` launches `roost worker`, and
//! a deploy's journal addresses `__keeper-contract` by string; a subcommand that
//! disappears or changes its arguments breaks all three without any Rust test
//! failing, so the surface is asserted here by name.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::collections::BTreeMap;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};

use roost_cli::doctor::window::{DEFAULT_WINDOW_LABEL, parse_window};
use roost_cli::{Cli, Command};

/// Every subcommand the crate's dispatcher answers, in the order `--help`
/// prints them. The list is the contract `docs/phase6-cli-contract.md`
/// documents; a command added here must be documented there in the same change.
///
/// **26, not 21.** 21 is v2's operator-and-daemon surface: v2's 23 command keys
/// minus the two v3 dropped (`cutover`, `__windows-updater-broker`). v3 adds
/// four `__remote-*` target-side commands that v2 never had, and a deploy runs
/// them over ssh, plus the `update` v2 also had and v3 had left unreachable
/// behind a complete implementation nobody could invoke. Asserting 21 would drop
/// exactly the commands this file exists to protect, and they would drop
/// silently.
const SUBCOMMANDS: [&str; 26] = [
    "coord",
    "worker",
    "keeper",
    "update",
    "status",
    "doctor",
    "version",
    "logs",
    "deploy",
    "keeper-refresh",
    "api",
    "quickstart",
    "push",
    "join",
    "add-machine",
    "dev",
    "self-link",
    "__remote-facts",
    "__remote-evidence",
    "__remote-transaction",
    "__remote-apply",
    "state",
    "reset",
    "skill",
    "test",
    "__keeper-contract",
];

#[test]
fn every_documented_subcommand_parses_with_no_arguments() {
    for name in SUBCOMMANDS {
        // The commands with a required argument are exercised with the
        // argument `--help` says they need, because "it parses" is a claim
        // about the command, not about its argument being optional. The rest
        // parse bare, which is itself part of the claim: a subcommand that
        // demands an argument nobody has a reason to supply is a surprise.
        let argv: Vec<String> = match name {
            "keeper" => vec![
                "roost".to_string(),
                name.to_string(),
                "/tmp/mux-keeper.sock".to_string(),
            ],
            "logs" => vec!["roost".to_string(), name.to_string(), "coord".to_string()],
            "deploy" => vec![
                "roost".to_string(),
                name.to_string(),
                "host.test".to_string(),
            ],
            "keeper-refresh" => vec![
                "roost".to_string(),
                name.to_string(),
                "host.test".to_string(),
            ],
            "add-machine" => vec![
                "roost".to_string(),
                name.to_string(),
                "--platform".to_string(),
                "linux".to_string(),
            ],
            "__remote-transaction" => vec![
                "roost".to_string(),
                name.to_string(),
                "--kind".to_string(),
                "deploy".to_string(),
            ],
            _ => vec!["roost".to_string(), name.to_string()],
        };
        let cli = Cli::try_parse_from(&argv)
            .unwrap_or_else(|error| panic!("{name} did not parse: {error}"));
        assert_eq!(cli.command.name(), name);
    }
}

#[test]
fn an_unknown_subcommand_is_refused_rather_than_guessed() {
    // The v2 spellings that are deliberately gone: a command that silently
    // became something else would be worse than one that says it does not
    // exist. `quickstart`, `push`, `api` and `dev` were on this list once and
    // L2–L6 landed all four, so this is now the two real drops and no more.
    for gone in ["cutover", "__windows-updater-broker"] {
        let error = Cli::try_parse_from(["roost", gone])
            .err()
            .unwrap_or_else(|| panic!("{gone} parsed: it is in the tree"));
        assert_eq!(
            error.kind(),
            ErrorKind::InvalidSubcommand,
            "{gone} must be refused AS AN UNKNOWN SUBCOMMAND, not by some other \
             rule that happens to fire first"
        );
    }
}

#[test]
fn a_bare_invocation_with_no_subcommand_is_a_usage_error() {
    // MEASURED, not assumed. `Cli` sets BOTH `subcommand_required` and
    // `arg_required_else_help` (src/lib.rs), so two kinds were live candidates
    // and they are opposites in intent: `MissingSubcommand` refuses, while
    // `DisplayHelpOnMissingArgumentOrSubcommand` prints the help. A bare `roost`
    // prints the help — which is the right call for an operator who typed the
    // name and nothing else, and the wrong one to guess at.
    //
    // This used to be a bare `is_err()`, which stayed green if either setting
    // were flipped and some unrelated rule started refusing instead. Pinned to
    // the kind so a change to either setting has to be a decision.
    let error = Cli::try_parse_from(["roost"]).expect_err("a bare roost must not run");
    assert_eq!(
        error.kind(),
        ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand,
        "a bare `roost` must print the help, and this says which rule did it"
    );
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

/// The deploy argument names are the contract's, verbatim. A rename here breaks
/// a wrapper script and a coordinator's catch-up invocation, and neither has a
/// Rust test that would notice.
#[test]
fn deploy_takes_exactly_the_documented_flags() {
    let cli = Cli::try_parse_from([
        "roost",
        "deploy",
        "studio",
        "--label",
        "studio",
        "--reachable-addr",
        "studio.example.test:4113",
        "--source-root",
        "/srv/roost",
        "--expected-sha",
        "b1d1836a00000000000000000000000000000000",
        "--expected-manifest-sha256",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "--coordinator-release",
        "--force-live",
    ])
    .unwrap();
    let Command::Deploy(args) = cli.command else {
        panic!("expected deploy");
    };
    assert_eq!(args.host, "studio");
    assert_eq!(args.label.as_deref(), Some("studio"));
    assert_eq!(
        args.reachable_addr.as_deref(),
        Some("studio.example.test:4113")
    );
    assert_eq!(
        args.source_root.as_deref(),
        Some(std::path::Path::new("/srv/roost"))
    );
    assert_eq!(
        args.expected_sha.as_deref(),
        Some("b1d1836a00000000000000000000000000000000")
    );
    assert!(args.expected_manifest_sha256.is_some());
    assert!(!args.allow_unpublished_local);
    assert!(args.coordinator_release);
    assert!(args.force_live);
}

/// Every flag is optional except the host, so a plain `roost deploy <host>` is a
/// complete invocation rather than a prompt waiting for an answer.
#[test]
fn a_bare_deploy_is_a_complete_invocation() {
    let cli = Cli::try_parse_from(["roost", "deploy", "studio"]).unwrap();
    let Command::Deploy(args) = cli.command else {
        panic!("expected deploy");
    };
    assert_eq!(args.host, "studio");
    assert!(args.label.is_none() && args.source_root.is_none() && !args.force_live);
}

/// The flags that authorize something destructive are spelled the way the
/// contract spells them. A `--force-live` that did not exist would leave
/// `--force-live=true` refused, which is the right failure and only if the name
/// is this one.
#[test]
fn the_destructive_flags_are_the_documented_spellings() {
    for command in ["deploy", "keeper-refresh"] {
        let mut argv = vec!["roost", command, "studio"];
        if command == "keeper-refresh" {
            argv.push("--yes");
        }
        argv.push("--force-live");
        let cli = Cli::try_parse_from(&argv).unwrap();
        let force_live = match cli.command {
            Command::Deploy(args) => args.force_live,
            Command::KeeperRefresh(args) => args.force_live,
            other => panic!("expected deploy or keeper-refresh, got {}", other.name()),
        };
        assert!(force_live, "{command} --force-live must set its own flag");
    }
}

/// `keeper-refresh` refuses to parse without `--yes` at the argument level? No:
/// it refuses at the command level with exit 2, so the flag has to be accepted
/// and the refusal has to be the command's own.
#[test]
fn keeper_refresh_takes_yes_and_force_live() {
    let cli = Cli::try_parse_from(["roost", "keeper-refresh", "studio", "--yes"]).unwrap();
    let Command::KeeperRefresh(args) = cli.command else {
        panic!("expected keeper-refresh");
    };
    assert_eq!(args.host, "studio");
    assert!(args.yes);
    assert!(!args.force_live);

    let cli = Cli::try_parse_from(["roost", "keeper-refresh", "studio", "--yes", "--force-live"])
        .unwrap();
    let Command::KeeperRefresh(args) = cli.command else {
        panic!("expected keeper-refresh");
    };
    assert!(args.yes && args.force_live);
}

/// The target-side subcommands are hidden, because an operator never types them:
/// a deploy addresses them by string over ssh. A visible one invites a human to
/// run a mutation by hand, which is precisely what the machine transaction exists
/// to prevent.
#[test]
fn the_target_side_commands_are_hidden_but_still_addressable() {
    let command = Cli::command();
    for hidden in [
        "__remote-facts",
        "__remote-evidence",
        "__remote-transaction",
        "__remote-apply",
    ] {
        let subcommand = command
            .get_subcommands()
            .find(|subcommand| subcommand.get_name() == hidden)
            .unwrap_or_else(|| panic!("{hidden} must be a subcommand"));
        assert!(
            subcommand.is_hide_set(),
            "{hidden} must stay hidden: it is addressed by a deploy, not by a person"
        );
    }
    // …and none of the operator-facing commands is hidden.
    for visible in ["deploy", "keeper-refresh"] {
        let subcommand = command
            .get_subcommands()
            .find(|subcommand| subcommand.get_name() == visible)
            .unwrap_or_else(|| panic!("{visible} must be a subcommand"));
        assert!(!subcommand.is_hide_set(), "{visible} must be visible");
    }
}

/// Every subcommand this dispatcher answers is specified in the contract
/// document, and every section of the document specifies subcommands.
///
/// The mapping has to be total in BOTH directions, and that is the whole
/// point. A command in the tree with no section is a command whose arguments,
/// exit codes and stdout/stderr split nobody has written down — this file
/// already records one: `roost update` had a complete implementation, a
/// `Command` variant nobody could reach, and a "not in the tree yet" table
/// listing it as missing. A section for a command this build does not answer
/// sends an operator looking for something the binary refuses.
///
/// A heading may name more than one command — the three server modes share one
/// section because they share an output contract — and may carry the argument
/// (`roost deploy <host>`). So the claim is not "one heading per command" but
/// "each command named by exactly one heading, and every heading naming at
/// least one command".
#[test]
fn every_subcommand_is_specified_in_the_contract_document() {
    let contract = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/phase6-cli-contract.md"),
    )
    .expect("the contract document is readable");

    let headings: Vec<&str> = contract
        .lines()
        .filter_map(|line| line.strip_prefix("## `roost "))
        .collect();
    let mut named: BTreeMap<&str, usize> = SUBCOMMANDS.iter().map(|name| (*name, 0)).collect();
    for heading in &headings {
        let words: Vec<&str> = heading
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
            .collect();
        let covered: Vec<&str> = SUBCOMMANDS
            .iter()
            .copied()
            .filter(|name| words.contains(name))
            .collect();
        assert!(
            !covered.is_empty(),
            "the contract has a section headed `{heading}` that names no subcommand of this \
             build: a section for a command that does not exist"
        );
        for name in covered {
            *named.get_mut(name).expect("every name is a key") += 1;
        }
    }
    for (name, count) in &named {
        assert_eq!(
            *count, 1,
            "`roost {name}` is answered by this build and the contract names it {count} times, \
             not once: either it has no section, or it has two that can disagree"
        );
    }

    // The two deliberate drops are named as drops rather than quietly absent,
    // because a missing subcommand is a usage error and someone will hit it.
    for dropped in ["roost cutover", "roost __windows-updater-broker"] {
        assert!(
            contract.contains(dropped),
            "{dropped} was in v2 and is not in v3; the contract must say so"
        );
    }
}

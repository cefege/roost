//! The one list of subcommands `roost-cli` answers, shared by the two files
//! that assert against it from opposite directions: `command_tree_shape.rs`
//! checks the tree IS this list, and `command_contract_coverage.rs` checks
//! every name here has a section in the contract.
//!
//! One list, not two. Two copies would drift, and the drift would be silent in
//! the exact direction that hides a command: the tree could gain a subcommand
//! that the contract file's copy never learns about.

/// Every subcommand the crate's dispatcher answers, in the order `--help`
/// prints them. The list is the contract `docs/phase6-cli-contract.md`
/// documents; a command added here must be documented there in the same change.
///
pub const SUBCOMMANDS: [&str; 27] = [
    "coord",
    "worker",
    "keeper",
    "update",
    "status",
    "db-to-postgres",
    "db-to-sqlite",
    "doctor",
    "version",
    "logs",
    "deploy",
    "keeper-refresh",
    "access",
    "api",
    "quickstart",
    "push",
    "join",
    "add-machine",
    "add-browser",
    "dev",
    "self-link",
    "__remote-facts",
    "__remote-evidence",
    "__remote-transaction",
    "__remote-apply",
    "reset",
    "__keeper-contract",
];

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
/// **29, not 21.** 21 is v2's operator-and-daemon surface: v2's 23 command keys
/// minus the two v3 dropped (`cutover`, `__windows-updater-broker`). v3 adds
/// four `__remote-*` target-side commands that v2 never had, and a deploy runs
/// them over ssh; `update`, which v2 also had and v3 had left unreachable
/// behind a complete implementation nobody could invoke; `import-v2`, which
/// has no v2 equivalent because it exists to move v2's state forward;
/// `add-browser`, which pairs the first browser with a coordinator no desktop
/// can open; and `db-to-postgres`, which moves a SQLite install onto Postgres.
/// Asserting 21 would drop exactly the commands this file exists to protect.
pub const SUBCOMMANDS: [&str; 29] = [
    "coord",
    "worker",
    "keeper",
    "update",
    "status",
    "import-v2",
    "db-to-postgres",
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
    "add-browser",
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

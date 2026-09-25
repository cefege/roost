//! The `roost` binary's command tree and subcommand dispatch, held in a
//! library so the argument parser is testable without spawning a process.
//! The binary's stdout is this program's product surface, which is why
//! roost-cli is the one crate `cargo xtask lint` exempts from the no-stdout
//! rule; every other crate logs through roost-observability.

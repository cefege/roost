//! Platform path and shell conventions: home-directory layout, executable suffixes, and per-platform data roots. No I/O, no async, no logging. Every crate that touches the filesystem depends on it instead of asking the OS directly.
//! Owned here until a phase fills it in; nothing calls it yet.

#![forbid(unsafe_code)]

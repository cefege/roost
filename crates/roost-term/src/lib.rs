//! The TerminalCore trait, its Alacritty-backed implementation, and the emitter that turns terminal state into a CellGridFrame wire message. Owns the subject side of the terminal-core conformance vectors.
//! Owned here until a phase fills it in; nothing calls it yet.

#![forbid(unsafe_code)]

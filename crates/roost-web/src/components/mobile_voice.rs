//! The composer's mic control: the component, the state it shares with its
//! callbacks, the send door, the shell command slot and the finalize watchdog.
//! Mounted by `terminal_chrome::composer_dictation`; every rule about what a tap
//! does is `crate::voice::state`.

pub mod context;
pub mod dom;
pub mod input;
pub mod send;
pub mod shell;
pub mod watchdog;

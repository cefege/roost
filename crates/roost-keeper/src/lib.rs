//! The keeper daemon: PTY ownership, per-channel byte rings, and the framed
//! keeper socket protocol specified in protocol/spec/keeper.md. Shipped as a
//! separate binary so a coordinator deploy never disturbs a live PTY.
//! This is the one crate outside third_party/ permitted to use `unsafe`: it
//! owns raw file descriptors and the controlling-TTY handshake, and every call
//! site must name the invariant it protects.

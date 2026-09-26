//! The pairing RPC surface: pair requests, approval, confirmation, and the
//! secrets a paired device receives. Owned by the pairing slice; the module
//! declaration lives in `auth/mod.rs` so a second slice editing the auth
//! surface cannot collide with it.

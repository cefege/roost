//! Everything that turns a presented credential into an answer.
//!
//! Owned by the coordinator. `authenticate` is the only entry point a transport
//! calls; the rest is split so each piece is testable alone: `jwt_claims` is the
//! token's shape, `jwt_crypto` the signature, `jwt_verify` the time and
//! revocation bounds, `jwt_key_cache` the generations, `principal` the roles, and
//! `authorized_keys` the only place a bearer meets the database.
//!
//! The split is not tidiness. `jwt_crypto` in particular is isolated so
//! `tests/jwt_parity.rs` can prove the primitive against tokens v2's own signer
//! produced, with no database and no clock in the way.

pub mod authenticate;
pub mod authorized_keys;
pub mod jwt_claims;
pub mod jwt_crypto;
pub mod jwt_key_cache;
pub mod jwt_verify;
pub mod principal;

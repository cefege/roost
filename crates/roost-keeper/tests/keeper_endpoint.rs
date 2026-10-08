#![cfg(unix)]
//! The keeper socket's security properties, which need no traffic to prove: the
//! endpoint's permissions, what may live at its path, and which half of a
//! leftover file is reclaimable.
//!
//! A keeper socket is a remote shell to every PTY the machine has open, so
//! these are properties of the filesystem rather than of a conversation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;

use roost_keeper::server::{Endpoint, ListenError, Server};

mod support;

use support::daemon::TempDir;

/// THE SECURITY PROPERTY. The socket must be 0600 from the moment it exists:
/// a listener that is briefly world-accessible is a window onto every PTY the
/// machine has open.
#[test]
fn the_socket_is_owner_only_before_anyone_connects() {
    let temp = TempDir::new("perms");
    let endpoint = Endpoint::new(temp.socket()).expect("endpoint");
    let _server = Server::bind(endpoint, temp.capability()).expect("bind");

    let mode = std::fs::metadata(temp.socket())
        .expect("the socket exists")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "the keeper socket is owner-only, not {mode:o}"
    );
}

/// A path that is not a socket must never be removed. A keeper that unlinks an
/// arbitrary file because it happened to be at the socket path destroys
/// whatever it actually was.
#[test]
fn a_path_that_is_not_a_socket_is_refused_not_removed() {
    let temp = TempDir::new("notasocket");
    std::fs::write(temp.socket(), b"important data").expect("a file at the path");

    // The refusal comes at CONSTRUCTION, not at bind: a path that is not a
    // socket should never be constructible as an endpoint at all.
    assert!(matches!(
        Endpoint::new(temp.socket()),
        Err(ListenError::NotASocket(_))
    ));
    assert_eq!(
        std::fs::read(temp.socket()).expect("the file is untouched"),
        b"important data",
        "a file that is not a socket must survive"
    );
}

/// A keeper that died leaves a socket file behind, because closing a listener
/// on Unix does not unlink its path. The next keeper must reclaim it, or one
/// crash becomes a permanent "address in use".
#[test]
fn a_stale_socket_from_a_dead_keeper_is_reclaimed() {
    let temp = TempDir::new("stale");
    // Binding and dropping leaves the socket file exactly as a crash does.
    drop(std::os::unix::net::UnixListener::bind(temp.socket()).expect("a raw socket"));
    assert!(
        temp.socket().exists(),
        "a dead listener leaves its socket file behind"
    );

    let endpoint = Endpoint::new(temp.socket()).expect("the path is constructible");
    let listener = endpoint
        .bind()
        .expect("a stale socket must be reclaimed, not refused");
    drop(listener);
}

/// The other half of the same rule: a socket a LIVE process still holds is not
/// stale, and replacing it would silently steal another keeper's PTYs.
#[test]
fn a_socket_a_live_keeper_holds_is_not_taken_over() {
    let temp = TempDir::new("inuse");
    let _live = std::os::unix::net::UnixListener::bind(temp.socket()).expect("a raw socket");

    let endpoint = Endpoint::new(temp.socket()).expect("the path is constructible");
    assert!(
        matches!(endpoint.bind(), Err(ListenError::AlreadyRunning(_))),
        "a keeper must not take over a socket a live process is serving"
    );
    assert!(temp.socket().exists(), "and must not remove it either");
}

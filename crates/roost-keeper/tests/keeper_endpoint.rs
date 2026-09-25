//! The keeper socket's security properties, which need no traffic to prove: the
//! endpoint's permissions, what may live at its path, and which half of a
//! leftover file is reclaimable.
//!
//! A keeper socket is a remote shell to every PTY the machine has open, so
//! these are properties of the filesystem rather than of a conversation.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use roost_keeper::server::{Endpoint, ListenError, Server};

mod support;

/// A socket path in a directory this test owns, removed when it goes out of
/// scope so a failed run cannot poison the next one.
struct TempSocket {
    dir: PathBuf,
    path: PathBuf,
}

impl TempSocket {
    fn new(label: &str) -> Self {
        let unique = format!(
            "roost-keeper-test-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let dir = std::env::temp_dir().join(unique.replace(['(', ')', ' '], ""));
        std::fs::create_dir_all(&dir).expect("a temp dir for the socket");
        let path = dir.join("keeper.sock");
        Self { dir, path }
    }
}

impl Drop for TempSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// THE SECURITY PROPERTY. The socket must be 0600 from the moment it exists:
/// a listener that is briefly world-accessible is a window onto every PTY the
/// machine has open.
#[test]
fn the_socket_is_owner_only_before_anyone_connects() {
    let temp = TempSocket::new("perms");
    let endpoint = Endpoint::new(temp.path.clone()).expect("endpoint");
    let _server = Server::bind(endpoint).expect("bind");

    let mode = std::fs::metadata(&temp.path)
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
    let temp = TempSocket::new("notasocket");
    std::fs::write(&temp.path, b"important data").expect("a file at the path");

    // The refusal comes at CONSTRUCTION, not at bind: a path that is not a
    // socket should never be constructible as an endpoint at all.
    assert!(matches!(
        Endpoint::new(temp.path.clone()),
        Err(ListenError::NotASocket(_))
    ));
    assert_eq!(
        std::fs::read(&temp.path).expect("the file is untouched"),
        b"important data",
        "a file that is not a socket must survive"
    );
}

/// A keeper that died leaves a socket file behind, because closing a listener
/// on Unix does not unlink its path. The next keeper must reclaim it, or one
/// crash becomes a permanent "address in use".
#[test]
fn a_stale_socket_from_a_dead_keeper_is_reclaimed() {
    let temp = TempSocket::new("stale");
    // Binding and dropping leaves the socket file exactly as a crash does.
    drop(std::os::unix::net::UnixListener::bind(&temp.path).expect("a raw socket"));
    assert!(
        temp.path.exists(),
        "a dead listener leaves its socket file behind"
    );

    let endpoint = Endpoint::new(temp.path.clone()).expect("the path is constructible");
    let listener = endpoint
        .bind()
        .expect("a stale socket must be reclaimed, not refused");
    drop(listener);
}

/// The other half of the same rule: a socket a LIVE process still holds is not
/// stale, and replacing it would silently steal another keeper's PTYs.
#[test]
fn a_socket_a_live_keeper_holds_is_not_taken_over() {
    let temp = TempSocket::new("inuse");
    let _live = std::os::unix::net::UnixListener::bind(&temp.path).expect("a raw socket");

    let endpoint = Endpoint::new(temp.path.clone()).expect("the path is constructible");
    assert!(
        matches!(endpoint.bind(), Err(ListenError::AlreadyRunning(_))),
        "a keeper must not take over a socket a live process is serving"
    );
    assert!(temp.path.exists(), "and must not remove it either");
}

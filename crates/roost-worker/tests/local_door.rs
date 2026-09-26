//! The local door's security core. Everything here is reachable from anything
//! on the machine, so what an unauthenticated peer can hold open is the whole
//! question, and each test pins one rule that answers it.

use std::time::{Duration, Instant};

use roost_worker::local_door::{
    Authenticated, Authentication, MAX_ESTABLISHED, PREHELLO_DEADLINE, PreHelloOwner, Refusal,
    sha256_of,
};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

fn owner() -> PreHelloOwner {
    PreHelloOwner::new()
}

fn grant_owner(owner: &mut PreHelloOwner, id: &str, secret: &str) {
    owner.install_grant(id, sha256_of(secret.as_bytes()), DAY, Instant::now());
}

/// Authenticate and insist on success. Most tests care that the door lets a
/// legitimate device in, and the failures that matter are the refusals, which
/// those tests assert directly.
#[track_caller]
fn admitted(result: Authentication) -> Authenticated {
    match result {
        Authentication::Authenticated(admitted) => admitted,
        Authentication::Refused(refusal) => panic!("expected admission, got {refusal:?}"),
    }
}

/// A socket that connects and says nothing is EXPIRED, not left to wait. A peer
/// that opens sockets and never authenticates must not be able to hold them.
#[test]
fn an_unauthenticated_socket_expires() {
    let start = Instant::now();
    let mut owner = owner();
    let socket = owner.next_socket_id();
    owner.admit(socket, start).expect("admitted");
    assert_eq!(owner.established(), 1);

    let just_inside = start + PREHELLO_DEADLINE - Duration::from_millis(1);
    assert!(owner.expire(just_inside).is_empty(), "not yet");

    let expired = owner.expire(start + PREHELLO_DEADLINE);
    assert_eq!(expired, vec![socket], "past the deadline it is released");
    assert_eq!(owner.established(), 0, "and the worker is not holding it");
}

/// The socket ceiling is enforced on ADMISSION, before any authentication. The
/// door is local, so a peer that cannot authenticate must not be able to
/// exhaust the worker.
#[test]
fn the_socket_ceiling_applies_before_authentication() {
    let now = Instant::now();
    let mut owner = owner();
    for _ in 0..MAX_ESTABLISHED {
        let socket = owner.next_socket_id();
        owner.admit(socket, now).expect("admitted");
    }
    let overflow = owner.next_socket_id();
    assert_eq!(
        owner.admit(overflow, now),
        Err(Refusal::AtCapacity),
        "the last socket is refused, authenticated or not"
    );
    assert_eq!(
        owner.established(),
        MAX_ESTABLISHED,
        "and the worker holds no more than its bound"
    );
}

/// The same socket cannot be admitted twice, or a peer could park one socket
/// against several deadlines.
#[test]
fn a_socket_is_admitted_once() {
    let now = Instant::now();
    let mut owner = owner();
    let socket = owner.next_socket_id();
    owner.admit(socket, now).expect("admitted");
    assert_eq!(owner.admit(socket, now), Err(Refusal::AlreadyAdmitted));
    assert_eq!(owner.established(), 1, "and it still holds exactly one");
}

/// THE REPLACE RULE. Replaying a grant REPLACES its socket rather than adding a
/// second: one grant is one terminal, and a second socket would be a second
/// sink for the same frames.
#[test]
fn replaying_a_grant_replaces_its_socket_rather_than_adding_one() {
    let now = Instant::now();
    let mut owner = owner();
    grant_owner(&mut owner, "grant-a", "secret-a");

    let first = owner.next_socket_id();
    match owner.authenticate("grant-a", b"secret-a", first, now) {
        Authentication::Authenticated(result) => {
            assert_eq!(result.replaced, None, "nothing to replace")
        }
        other => panic!("expected admission, got {other:?}"),
    }

    // The device reconnects on a new socket.
    let second = owner.next_socket_id();
    match owner.authenticate("grant-a", b"secret-a", second, now) {
        Authentication::Authenticated(result) => {
            assert_eq!(
                result.replaced,
                Some(first),
                "and the prior socket is REPORTED so the caller closes it"
            );
        }
        other => panic!("expected admission, got {other:?}"),
    }

    assert_eq!(
        owner.established(),
        1,
        "one grant is one terminal: the prior socket is gone, not merely forgotten"
    );
    assert_eq!(
        owner.grant_for(first),
        None,
        "the replaced socket no longer holds the grant"
    );
    assert_eq!(
        owner.grant_for(second),
        Some("grant-a"),
        "and the new one does"
    );
}

/// Authenticating on the socket that already holds the grant is not a
/// replacement — it is the same socket re-authenticating, and reporting a
/// replacement there would make the caller close the socket it just admitted.
#[test]
fn reauthenticating_the_same_socket_replaces_nothing() {
    let now = Instant::now();
    let mut owner = owner();
    grant_owner(&mut owner, "grant-a", "secret-a");
    let socket = owner.next_socket_id();

    admitted(owner.authenticate("grant-a", b"secret-a", socket, now));
    match owner.authenticate("grant-a", b"secret-a", socket, now) {
        Authentication::Authenticated(result) => {
            assert_eq!(
                result.replaced, None,
                "the same socket is not its own replacement"
            );
        }
        other => panic!("expected admission, got {other:?}"),
    }
    assert_eq!(owner.established(), 1);
}

/// THE SECRET IS NEVER STORED. The coordinator installs a digest; the worker
/// compares digests. A bug in this process therefore cannot leak a credential
/// it does not have, and the type reflects that.
#[test]
fn a_wrong_secret_is_refused_and_a_right_one_is_not() {
    let now = Instant::now();
    let mut owner = owner();
    grant_owner(&mut owner, "grant-a", "the-real-secret");
    let socket = owner.next_socket_id();

    assert_eq!(
        owner.authenticate("grant-a", b"a-guess", socket, now),
        Authentication::Refused(Refusal::BadSecret),
        "a wrong secret is refused"
    );
    assert_eq!(
        owner.grant_for(socket),
        None,
        "and the socket holds nothing"
    );

    assert!(matches!(
        owner.authenticate("grant-a", b"the-real-secret", socket, now),
        Authentication::Authenticated(_)
    ));
}

/// A wrong secret and an unknown grant are DIFFERENT, because they are
/// different problems: one is a stale authorization, the other a bad
/// credential, and an operator reads them differently.
#[test]
fn a_wrong_secret_and_an_unknown_grant_are_distinguishable() {
    let now = Instant::now();
    let mut owner = owner();
    grant_owner(&mut owner, "grant-a", "the-real-secret");
    let socket = owner.next_socket_id();

    assert_eq!(
        owner.authenticate("no-such-grant", b"the-real-secret", socket, now),
        Authentication::Refused(Refusal::UnknownGrant)
    );
    assert_eq!(
        owner.authenticate("grant-a", b"a-guess", socket, now),
        Authentication::Refused(Refusal::BadSecret)
    );
}

/// ACTIVE EXPIRY. A connected carrier must not outlive its authorization just
/// because the coordinator is unreachable to renew it — an unreachable
/// coordinator is exactly when a stale grant is most dangerous.
#[test]
fn a_grant_expires_even_though_it_is_in_use() {
    let start = Instant::now();
    let mut owner = owner();
    owner.install_grant(
        "grant-a",
        sha256_of(b"secret"),
        Duration::from_secs(10),
        start,
    );
    let socket = owner.next_socket_id();
    admitted(owner.authenticate("grant-a", b"secret", socket, start));

    // Past its expiry, the same grant no longer authenticates.
    let later = start + Duration::from_secs(11);
    let reconnect = owner.next_socket_id();
    assert_eq!(
        owner.authenticate("grant-a", b"secret", reconnect, later),
        Authentication::Refused(Refusal::UnknownGrant),
        "an expired grant authenticates nothing, however recently it was used"
    );
}

/// An expired grant nobody presents is still an entry this process is holding,
/// so it is swept.
#[test]
fn an_unused_expired_grant_is_swept() {
    let start = Instant::now();
    let mut owner = owner();
    owner.install_grant("short", sha256_of(b"secret"), Duration::from_secs(5), start);
    owner.install_grant("long", sha256_of(b"secret"), DAY, start);

    assert_eq!(
        owner.expire_grants(start + Duration::from_secs(6)),
        1,
        "the expired one went"
    );
    let socket = owner.next_socket_id();
    assert!(
        matches!(
            owner.authenticate("long", b"secret", socket, start + Duration::from_secs(6)),
            Authentication::Authenticated(_)
        ),
        "and the live one still works"
    );
}

/// A closed socket frees its grant, so a reconnecting device is not refused for
/// a limit it is no longer occupying.
#[test]
fn closing_a_socket_frees_its_grant() {
    let now = Instant::now();
    let mut owner = owner();
    grant_owner(&mut owner, "grant-a", "secret");
    let socket = owner.next_socket_id();
    admitted(owner.authenticate("grant-a", b"secret", socket, now));
    assert_eq!(owner.established(), 1);

    owner.close(socket);
    assert_eq!(
        owner.established(),
        0,
        "the worker holds nothing for a closed socket"
    );

    // And the grant can be used again from scratch.
    let again = owner.next_socket_id();
    match owner.authenticate("grant-a", b"secret", again, now) {
        Authentication::Authenticated(result) => {
            assert_eq!(
                result.replaced, None,
                "the closed socket is not reported as replaced"
            )
        }
        other => panic!("expected admission, got {other:?}"),
    }
}

/// Authenticating clears the socket's Hello wait, so an authenticated socket is
/// not also counted against the pre-Hello deadline.
#[test]
fn authenticating_clears_the_hello_wait() {
    let start = Instant::now();
    let mut owner = owner();
    grant_owner(&mut owner, "grant-a", "secret");
    let socket = owner.next_socket_id();
    owner.admit(socket, start).expect("waiting for a hello");
    assert_eq!(owner.established(), 1);

    admitted(owner.authenticate("grant-a", b"secret", socket, start));
    assert_eq!(
        owner.established(),
        1,
        "one socket, counted once, not twice"
    );
    assert!(
        owner.expire(start + PREHELLO_DEADLINE * 2).is_empty(),
        "and it is no longer expiring as unauthenticated"
    );
}

/// The digest is a real SHA-256, because a grant secret is a bearer credential
/// and a stand-in digest would make every test pass and every deployment
/// insecure.
#[test]
fn the_grant_digest_is_really_sha256() {
    // The published digest of the empty string. A weaker or hand-rolled hash
    // would not produce it.
    assert_eq!(
        sha256_of(b""),
        [
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
            0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
            0x78, 0x52, 0xb8, 0x55
        ]
    );
    assert_ne!(
        sha256_of(b"a"),
        sha256_of(b"b"),
        "and distinct inputs give distinct digests"
    );
}

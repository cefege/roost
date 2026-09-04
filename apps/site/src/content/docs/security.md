---
title: "Security model"
description: "How Roost uses non-extractable device keys in both deployment modes, plus self-hosted pairing, revocation, audit, backups, and telemetry behavior."
order: 9
section: "Reference"
---

Managed Roost is a private operator-provisioned service with one initial owner
and dashboard. The owner password authorizes a browser's non-extractable device
key; subsequent requests use signed device JWTs. Self-hosted startup instead
creates and validates one internal local account, `personal` organization, and
`default` dashboard automatically. That topology is not an operator login
identity; the pairing and operational details below describe self-hosted access.

## Self-hosted device identity is a key, not a login account

Self-hosted Roost has no login-account provisioning and no shared tokens. Each
browser mints its own Ed25519 key pair with WebCrypto, marks it
**non-extractable**, and persists it through IndexedDB's structured clone. A
non-extractable private key cannot be exported by
the page that created it, so there is no code path — including Roost's own — that
reads it out and sends it anywhere.

Every request carries an EdDSA JWT signed by that key. The coordinator verifies
it with Bun's WebCrypto:

- the header `alg` must be `EdDSA`; anything else is rejected;
- the `kid` is the lowercase hex SHA-256 of the raw 32-byte public key, and that
  same value is the device's fingerprint everywhere in the UI and the CLI;
- the `aud` for inbound browser tokens is `roost-coordinator`;
- a `kid` that is not an authorized key is a 401, and the public-key cache is
  generation-checked on every lookup so a revoked key cannot be served from a
  warm cache;
- tokens are short-lived — the default maximum accepted age is 300 seconds.

Key rotation keeps the old and the staged key until the coordinator's commit state
is unambiguous, so a rotation interrupted midway cannot lock a device out.

## Three ways to authorize a browser

**1. QR pairing.** In **Settings → Pair a device**, Roost mints a one-shot browser
bootstrap token and renders a QR for the current HTTPS origin. The token rides in
the URL **fragment**, which browsers never send to the server — so it cannot land
in the coordinator's request log, a proxy or Cloudflare access log, or a `Referer`
header. Scan it with a phone camera and the device signs itself in.

**2. Paste a bootstrap token.** Mint a token in an already-authorized browser and
paste it into the new one.

**3. Tap-to-pair approval.** The new browser posts its public key and polls for a
decision. An already-authorized browser sees the request under **Pending pair
requests** and approves or denies it; on approval the waiting browser reloads
itself with an authorized token. Nothing is typed on either side.

Neither loopback nor a Tailscale address authorizes a browser. Those addresses
are transport metadata only; a fresh device still needs a scoped one-shot grant
or an explicit pairing approval.

Bootstrap tokens are prefixed `roost_bt_`, single-use, and expire 24 hours after
minting whether or not they are redeemed. Redemption claims the row atomically —
matching only unused, unexpired tokens — so a token cannot be spent twice or
replayed after expiry.

## Revoking a device

Delete the device's row. Its `kid` stops verifying immediately, because the cache
is generation-checked rather than TTL-only. A device cannot revoke *itself* — that
request is refused with a pointer at key rotation, so a compromised browser cannot
lock you out by revoking the credential you are holding.

If you have lost every authorized browser, revoke from the coordinator host:

```sh
roost api device-revoke-local <fingerprint> --yes
```

It only accepts an `http://127.0.0.1:<port>` coordinator URL with no credentials,
path, query, or fragment. See [the CLI](/docs/cli/).

## Enrollment boundaries

Worker enrollment uses the same one-shot bootstrap tokens, minted by
`roost add-machine` or **Settings → Machines → Add machine**, and the machine
joins by *pulling* — the coordinator never SSHes out to push a credential. On
Windows, nothing downloaded is executed until its Authenticode chain, trusted
timestamp, and exact leaf-certificate pin have been verified against a publisher
fingerprint you supplied out of band, and the signed join script keeps the one-shot
token in memory only.

Workers dial the coordinator outbound and never listen, so a worker machine
exposes no inbound port to attack. Windows services run under a dedicated
low-privilege `roost-operator` identity that is denied interactive logon;
administrator identities are rejected outright.

## The audit log

Every Connect RPC is audited in the authentication interceptor — the only layer
that has both the verified caller fingerprint and the response status. Each row
records the method, path, status, trace id, and caller fingerprint. Non-Connect
paths are audited in the outer request wrapper with a null caller, because there
is no JWT context there.

High-frequency, zero-signal methods are skipped **only when they succeed**: health
probes, worker heartbeats, pair-list polling, resize and cursor chatter, the
non-mutating list reads the app polls, and the receipt for the app uploading its
own debug logs. A non-200 for any of them is always written, because a failing
heartbeat or a rejected health probe is exactly the anomaly worth keeping.

Retention is an explicit allowlist, not a blanket age cutoff. A sweep runs at
startup and every 24 hours and deletes **only** `SessionsInput` rows — "who typed
into which session", genuine audit data but by far the highest volume — older than
the retention window, which defaults to 90 days and is configurable through
`ROOST_COORDINATOR_AUDIT_RETENTION_DAYS`. Deletion runs in 10,000-row batches with
a yield between statements, so a large backlog cannot block live RPCs on the
coordinator's single write thread.

Everything with forensic value is kept indefinitely: `PairApprove`,
`AuthRedeemBrowser`, `WorkersDelete`, `WorkspacesDelete`, `SessionsKill`, and
`SessionsSpawn`. "When was this device authorized, and by whom" is precisely the
question the log exists to answer.

## Backups

The coordinator writes a verified SQLite snapshot before applying pending
migrations to an existing database, and again on a 24-hour interval. Each snapshot
is integrity-checked as a standalone database before being compressed, and the 14
newest `coord_v2.<timestamp>.db.gz` archives are retained in a `backups/`
directory beside the database, created with owner-only permissions.

These are same-host rollback material. They do not survive the loss of the
coordinator's disk and are not off-host disaster recovery; copy them to storage
with an independent failure domain if host-loss recovery matters.

## What the public surface refuses

In the self-hosted edition, if you enable the optional Cloudflare browser endpoint, the public listener is
strictly narrower than the main one. It returns 404 for anything under
`/internal/`, for the worker WebSocket path, for `/api/db-export`, and for the
worker-redemption and coordinator-relocation RPCs. Cloudflare Access
authenticates a human; a browser still needs a scoped one-shot grant or an
approved pairing request. Details in [networking](/docs/networking/).

## Self-hosted telemetry behavior

The self-hosted edition has no analytics, crash reporting, or phone-home, and no
Roost vendor account is involved. Diagnostics are local files: always-on signal
events land in the coordinator's and worker's own error logs, and
`roost doctor --since <window>` summarizes them from disk. Agent status is not
persisted at all, so there is not even a local history of what you were running.
Data leaves your hardware only through integrations you deliberately configure —
for example dictation sent to Deepgram or a Cloudflare tunnel you operate.

## Next

- [Networking](/docs/networking/) — the private path and the public deny list
- [The CLI](/docs/cli/) — `api device-revoke-local`, `status`, `doctor`
- [Fleet](/docs/fleet/) — event log, backups, and fleet updates
- [Quickstart](/docs/quickstart/) — pairing in practice

<!-- Auth contract: non-extractable Ed25519 device keys, coordinator JWT verification, and pairing ceremony. -->
<!-- Pairing grants authority only after token-bound confirmation; URLs never carry accepted query credentials. -->
<!-- roost_protocol::coordinator_jwt owns the JWT shape; roost-client-core's auth module owns portable ceremony entropy and canonical validation. -->

# Authentication and pairing

## Purpose

Roost authenticates devices and workers by public Ed25519 keys. A browser keeps its private key non-extractable, signs short-lived coordinator JWTs locally, and sends the bearer in `Authorization` or the second WebSocket subprotocol. Pairing is the explicit ceremony that adds a browser key to the account; approval alone grants no authority.

## Messages

| Message | File | Meaning |
| --- | --- | --- |
| JWT header/payload | `crates/roost-protocol/src/coordinator_jwt.rs` | `alg=EdDSA`, `typ=JWT`, `kid=<fingerprint>`; payload repeats fingerprint as `sub`, audience `roost-coordinator`, and `iat`/`exp`. |
| `PairCreateRequest/Response`, `PairPollRequest/Response` | `protocol/proto/roost/v1/coordinator.proto` | Anonymous requester creates a token-bound request and polls status. |
| `PairListRequest/Response`, `PairApproveRequest/Response` | `protocol/proto/roost/v1/coordinator.proto` | Authorized approver lists pending requests and binds a verification code. |
| `PairConfirmRequest/Response`, `PairDenyRequest/Response` | `protocol/proto/roost/v1/coordinator.proto` | Requester confirms with ID/token/code; approver denies. |
| `PairApprovalStatusRequest/Response` | `protocol/proto/roost/v1/coordinator.proto` | Secret-free progress for the exact approver. |
| `AuthMintBootstrapRequest/Response`, `AuthRedeemWorkerRequest/Response`, `AuthRedeemBrowserRequest/Response` | `protocol/proto/roost/v1/coordinator.proto` | Authenticated one-shot bootstrap mint and public redemption into a persisted principal. |
| `DevicesList/Revoke/RotateCurrent` messages | `protocol/proto/roost/v1/coordinator.proto` | Device inventory and key lifecycle. |

## State machine
1. Browser WebCrypto generates Ed25519 with `extractable=false`. IndexedDB owns atomic key lifecycle. The fingerprint is lowercase SHA-256 hex of the raw 32-byte public key.
2. For each protected request, browser signs `{sub:kid,aud:"roost-coordinator",iat,exp}`. Connect sends `Authorization: Bearer <jwt>` and `x-roost-tab-id`; WebSockets send the JWT as their second subprotocol. The coordinator verifies signature, algorithm, audience, `sub===kid`, time bounds, and current key generation, then resolves exactly one account-device, worker, or legacy-self-hosted principal.
3. `PairCreate` validates ceremony version, Ed25519 public key, 16-byte request ID, and 32-byte requester token. If Cloudflare Access is enabled for a non-on-host request, valid front-door identity is required. It creates an exact-idempotent, expiring `pending` row and publishes only secret-free metadata.
4. `PairPoll` requires the matching requester-token digest. An authorized/on-host approver lists and approves with a six-digit code, moving `pending → verification_required`.
5. `PairConfirm` alone revalidates requester token, version, expiry, requester-key availability, approver/account continuity, and key collisions. It then transactionally creates the authorized key/account-device association and moves `verification_required → completed`. Wrong nonterminal codes return `{ok:false}` and increment the bounded attempt count; the terminal transition is `verification_failed`.
6. `PairApprovalStatus` is exact-approver-only and secret-free. `PairDeny` moves either live state to `denied`. Expiry retention terminalizes live rows, clears code digests, then removes terminal rows later.
7. URL bootstrap accepts only one nonempty fragment `pair` value. Startup captures it into tab/session-scoped storage and synchronously scrubs query/fragment credentials before network-facing modules load. Query-shaped credentials are scrubbed but never accepted.

## Limits

| Constant | Value | Rust source |
| --- | ---: | --- |
| `COORDINATOR_JWT_LIFETIME_SECS` | `300 s` | `crates/roost-protocol/src/coordinator_jwt.rs` |
| `JWT_CACHE_TTL_MS` | `240,000 ms` | `crates/roost-client-core/src/client/auth/jwt.rs` |
| `PAIRING_CEREMONY_VERSION` | `1` | `crates/roost-client-core/src/client/auth/ceremony.rs` |
| `PAIR_VERIFICATION_CODE_LENGTH` | `6` decimal digits | `crates/roost-client-core/src/client/auth/ceremony.rs` |
| `PAIR_REQUEST_ID_BYTES` | `16 bytes / 32 lowercase hex` | `crates/roost-client-core/src/client/auth/ceremony.rs` |
| `PAIR_REQUESTER_TOKEN_BYTES` | `32 bytes / 64 lowercase hex` | `crates/roost-client-core/src/client/auth/ceremony.rs` |
| `PAIR_REQUEST_TTL_MS` | `600,000 ms` | `crates/roost-coord/src/auth/pairing/secrets.rs` |
| `MAX_PENDING_PAIR_REQUESTS` | `32` | `crates/roost-coord/src/auth/pairing/secrets.rs` |
| `PAIR_VERIFICATION_ATTEMPT_LIMIT` | `5` | `crates/roost-coord/src/auth/pairing/secrets.rs` |
| Bootstrap token | `BOOTSTRAP_TOKEN_PREFIX` (`roost_bt_`) + `48` lowercase hex; `BOOTSTRAP_TOKEN_TTL_MS` = `86,400,000 ms` | `crates/roost-coord/src/auth/bootstrap_tokens.rs` |

## Errors

- A protected handler without a browser principal returns Connect `Unauthenticated: authentication required` with `x-roost-auth-layer=device` (`crates/roost-coord/src/auth/principal.rs`, `AUTH_LAYER_HEADER`; the gate is `crates/roost-coord/src/rpc/auth_gate.rs`).
- Invalid request ID/token/key or malformed verification code returns `InvalidArgument`; ceremony mismatch returns `FailedPrecondition: pairing client must reload`.
- Missing front-door assertion returns `Unauthenticated: pairing requires front-door sign-in`. Poll/confirm deliberately use generic `NotFound` for unknown or foreign request/token evidence.
- Pair conflicts use `AlreadyExists`; revoked keys/approver continuity use `PermissionDenied`; non-pending/expired/terminal transitions use `FailedPrecondition`; the 32-request ceiling uses `ResourceExhausted`.
- Long-lived authenticated sockets close with code `4003`, reason `reauth required`, at the verified token deadline. Key-generation revocation closes with `4001` (`crates/roost-coord/src/auth/ws_auth_deadline.rs`).

## Reference implementation

JWT shape `crates/roost-protocol/src/coordinator_jwt.rs`; browser identity, signing and fragment boundary `crates/roost-client-core/src/client/auth/` with the WebCrypto vault in `crates/roost-web/src/platform/device_key/` and `crates/roost-web/src/platform/fragment_credential.rs`; coordinator verification and principal `crates/roost-coord/src/auth/` (`jwt_verify.rs`, `principal.rs`) with the gate in `crates/roost-coord/src/rpc/auth_gate.rs`; pairing `crates/roost-coord/src/auth/pairing/`; bootstrap and device lifecycle `crates/roost-coord/src/auth/rpc_bootstrap.rs` and `crates/roost-coord/src/auth/rpc_devices.rs`.

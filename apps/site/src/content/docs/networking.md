---
title: "Networking"
description: "One loopback coordinator listener, the front door you choose, and the paths that stay private."
order: 7
section: "Reference"
---

## One listener, one declared origin

The coordinator binds loopback and speaks plaintext:

```text
your front door  →  127.0.0.1:4103
```

That is the whole network model. TLS, DNS, tunnels, and public reachability
belong to whatever you put in front of it, and you tell the coordinator the
resulting origin:

```sh
roost quickstart --coordinator-url https://roost.example.com
```

Quickstart installs the coordinator with `ROOST_COORDINATOR_BIND=127.0.0.1:4103`,
`ROOST_TRUST_PROXY=1`, and `ROOST_WEB_PUBLIC_URL=https://roost.example.com`.
`--coordinator-url` must be an absolute `https:` origin with no path, query, or
fragment; an explicit port is optional. Roost never provisions a certificate,
never registers DNS, and never invents an origin for you.

`roost status` reports that configured URL and whether it answers.

## Three front doors

Any reverse proxy works. These three cover most installs, and Roost implements
none of them — copy-paste configuration lives in
[Install](/docs/install/) and the repository's `GETTING_STARTED.md`.

| | Caddy with your domain | Cloudflare tunnel | `tailscale serve` |
|---|---|---|---|
| Public DNS/domain | yours | yours, on Cloudflare | none |
| Inbound ports open | 80/443 | none | none |
| Certificate on the box | Caddy manages it | none | Tailscale manages it |
| Browser device software | ordinary browser | ordinary browser | Tailscale client |
| Best reason to choose it | plain public hosting | works behind NAT | no domain, no exposure |

Workers dial the coordinator origin outbound and never listen for inbound
connections. When workers should use a different door than browsers, set
`ROOST_COORDINATOR_PUBLIC_URL` to that origin; enrollment resolves
`ROOST_COORDINATOR_URL` → `ROOST_COORDINATOR_PUBLIC_URL` →
`ROOST_WEB_PUBLIC_URL` and refuses when none is set.

## Overwrite `X-Forwarded-For`, never append

The coordinator reads the **first** entry of `X-Forwarded-For` as the caller
address, and that address decides whether a request counts as on-host. Your
front door must therefore replace the header with its own client address.
A proxy that appends — `cloudflared` appends the visitor address after any
client-supplied chain — lets a caller prepend an address of its choosing and
claim to be on-host.

Prove it from a machine that is not the coordinator:

```sh
curl -si https://roost.example.com/api/db-export | head -1
```

`403` is correct, and `404` is equally correct once you deny the path at the
front door as below. Only `200` is a failure: it means the caller address is
not arriving intact and the whole database is downloadable.

## Paths that stay private

Deny two prefixes at the front door: `/internal/*`, a namespace the coordinator
reserves for private on-host routes, and `/api/db-export` (the whole database).
The coordinator also refuses `/api/db-export` for any caller it does not resolve
as on-host, so the edge rule is defence in depth.

`/ws/coord-worker/*` — the worker link — passes by default: in a standard
install workers dial the same origin browsers use. Deny it only once workers
have their own declared origin (`ROOST_COORDINATOR_PUBLIC_URL` on the
coordinator, matching `ROOST_COORDINATOR_URL` on each worker). Denying it while
workers still dial the public origin strands every one of them on a 404
transport.

## No network position grants authority

Being on the tailnet, on a VPN, or on loopback is transport reachability, never
authorization. Every browser redeems a scoped one-shot grant or completes an
approved pairing request; every worker redeems a scoped worker grant. A login
your front door performs is authentication of a human, not authorization of a
device.

WireGuard, Headscale, ZeroTier, and other private overlays make the front door
reachable and change nothing else; Roost neither configures nor exercises them.

## Next

- [Install](/docs/install/) — the one install shape and the front-door recipes
- [Fleet](/docs/fleet/) — why workers only ever dial outbound
- [Security](/docs/security/) — pairing, device keys, and revocation
- [The CLI](/docs/cli/) — `status`, `doctor`, and the rest

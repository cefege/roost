---
title: "Networking"
description: "Tailscale Serve is the supported topology. Optional Cloudflare Access for browser-only devices, what the public listener refuses, and what stays manual."
order: 7
section: "Reference"
---

## The supported topology: Tailscale Serve

The coordinator binds a loopback listener on `127.0.0.1:4103`. Tailscale Serve
publishes it on the tailnet as `:4102`. That single mapping —

```text
Tailscale Serve :4102  →  127.0.0.1:4103
```

— is the topology the installer configures, the one `roost quickstart` produces,
and the only one the release canaries exercise. Everything below about other
networks is honest about being outside that boundary.

Tailscale is doing three jobs here, which is why it is a prerequisite rather than
a suggestion. It is the private transport between browsers, the coordinator, and
every worker. It is the trusted enrollment boundary: a worker's join and a
browser's self-authorization are accepted only from loopback or a tailnet peer.
And it supplies TLS — `roost quickstart` mints the certificate for the machine's
tailnet FQDN, so browsers get real HTTPS with no self-signed warnings and phones
connect with no port forwarding.

Workers dial the coordinator's tailnet URL outbound
(`https://<coord>.<tailnet>.ts.net:4102`) and never listen for inbound
connections. `roost status` reports whether TLS is currently provided by Tailscale
Serve or by a direct certificate.

## Optional: Cloudflare browser access

Cloudflare Access adds a *public browser endpoint* on top of an already working
Roost installation. It exists for one case: reaching your fleet from a device you
cannot or will not install a VPN client on — a work laptop, a borrowed machine, a
locked-down phone.

What changes and what does not:

| | Default Tailscale path | Cloudflare browser access |
|---|---|---|
| Browser device software | Tailscale app | Ordinary browser only |
| Coordinator/worker network | Tailscale | Tailscale |
| Public DNS/domain | None | Cloudflare-managed domain required |
| Cloudflare setup | None | Tunnel plus a self-hosted Access application |
| Tradeoff | Browser must join the tailnet | Manual Cloudflare administration and an extra internet-facing dependency |

Only the coordinator runs `cloudflared`; workers and browsing devices do not
install it. The coordinator opens a second loopback listener on
`127.0.0.1:4104` for this path, and `roost expose` refuses to run when
`cloudflared` is not on `PATH`.

The tunnel ingress must point at that listener:

```yaml
tunnel: <TUNNEL-UUID>
credentials-file: /absolute/path/printed/by/cloudflared/<TUNNEL-UUID>.json
ingress:
  - hostname: roost.example.com
    service: http://127.0.0.1:4104
  - service: http_status:404
```

Then tell Roost about the Access application:

```sh
roost expose roost.example.com \
  --team <team>.cloudflareaccess.com \
  --aud <64-lowercase-hex>
```

Add `--config <path>` for a non-default config; a relative path is resolved
against the current working directory before validation.

`roost expose` validates before it changes anything. The hostname must be a bare
FQDN with no scheme, port, or path. `--team` must match
`<team>.cloudflareaccess.com`, and `--aud` must be exactly 64 lowercase hex
characters. The config must exist and pass `cloudflared`'s own ingress
validation. The **first** ingress rule must be pathless and name exactly your
hostname and `http://127.0.0.1:4104`. And four representative URLs — the site
root, `/api/db-export`, `/internal/coord-handoff/commit`, and
`/ws/coord-worker/<fp>` — are each resolved through `cloudflared` and must land on
that same service, so a stray later rule cannot quietly capture one of them.

`roost expose` prints the service-install command to run afterwards; with the
default config it is
`sudo cloudflared --config "$HOME/.cloudflared/config.yml" service install`. The
explicit `--config` matters on Linux, where `sudo` gives the service `$HOME=/root`
and `cloudflared` would otherwise not find the config you just wrote. Roost does
not install, update, or own that Cloudflare service; it only prints the command.

## What the public listener refuses

Access authenticating a human is not the same as authorizing a device, and the
public surface is narrower than the private one by construction. Even behind a
correct Access policy, the public listener returns 404 for a fixed deny list:

- any path under `/internal/`
- `/ws/coord-worker/…` — the worker transport
- `/api/db-export`
- the RPCs `AuthAuthorizeBrowser`, `AuthRedeemWorker`,
  `AuthMintCoordinatorRelocation`, `AuthRedeemCoordinatorRelocation`,
  `CoordinatorMovePreflight`, `CoordinatorMoveStart`, `CoordinatorMoveStatus`,
  and `MiscDbExportUrl`

So worker enrollment, worker transport, database export, internal handoff, and
coordinator relocation stay on the private Tailscale path whether or not the
public endpoint exists.

Roost pairing still authorizes the browser as a Roost device. A successful Access
login without pairing is not sufficient. To verify the edge before you trust it,
log out of Access and run:

```sh
curl -i -X POST https://roost.example.com/roost.v1.CoordinatorService/MiscHealth
```

Expect an Access login redirect or challenge. An origin HTTP 200 means Access is
absent; 502 or 530 means tunnel routing is wrong.

Also worth knowing before you enable it: `roost deploy`, VNC/Screen Sharing,
Finder/SMB, SSH/rsync, and development-port links keep using direct or tailnet
reachability. And Cloudflare availability plus your Access policy become
dependencies of the public URL — the private Tailscale path remains available
regardless.

## Turning public access off

Stop and remove the tunnel service, then delete the Access application, the DNS
record, and the tunnel in the Cloudflare dashboard:

```sh
sudo systemctl disable --now cloudflared   # Linux only
sudo cloudflared service uninstall
```

There is no `roost unexpose`. `roost expose` persists the public URL and the
Access settings into the coordinator service, so deleting the Cloudflare resources
removes public reachability but does not restore local pairing links or clear the
saved public URL. Do not hand-edit the launchd or systemd definitions; reinstall
or reconfigure the coordinator for a full local reset.

## Everything else is manual and unexercised

WireGuard, Headscale, ZeroTier, other VPNs, and a plain LAN can be wired up by
hand once browser-to-coordinator and worker-to-coordinator reachability is already
solved. Roost has no opinion that stops you.

They are not supported paths, though, and the docs will not pretend otherwise: the
installer does not configure them, and the release canaries do not exercise them.
If you go that route, you own the transport, the certificates, and the enrollment
boundary that Tailscale would otherwise provide.

## Next

- [Install](/docs/install/) — the Tailscale prerequisite in order
- [Fleet](/docs/fleet/) — why workers only ever dial outbound
- [Security](/docs/security/) — pairing, device keys, and revocation
- [The CLI](/docs/cli/) — `expose`, `status`, `doctor`

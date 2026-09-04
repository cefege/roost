---
title: "Networking"
description: "Automatic Tailscale Serve and explicit-certificate HTTPS topologies, optional Cloudflare Access, and the public listener boundary."
order: 7
section: "Reference"
---

## Coordinator HTTPS: automatic Tailscale Serve or an explicit certificate

`roost quickstart` supports two mutually exclusive endpoint modes. With no
endpoint flags, automatic mode configures Tailscale Serve in front of a loopback
coordinator:

```text
Tailscale Serve :4102  →  127.0.0.1:4103
```

For a host reachable without Tailscale, explicit mode serves HTTPS directly:

```sh
roost quickstart \
  --coordinator-url https://roost.example.com:4102 \
  --tls-cert /absolute/path/fullchain.pem \
  --tls-key /absolute/path/privkey.pem
```

The URL, certificate, and key flags form one required group; partial input never
falls back to Tailscale. The certificate must be browser-trusted for the
coordinator hostname.

Tailscale supplies convenient private reachability and TLS in automatic mode. It
is never an application identity or enrollment authority. Every browser must
redeem a scoped one-shot browser grant or complete approved pairing, and every
worker must redeem a scoped worker grant; a loopback or tailnet source address
does not replace either credential.

Workers dial the configured coordinator HTTPS origin outbound and never listen
for inbound connections. `roost status` reports whether TLS is provided by
Tailscale Serve or directly by the coordinator certificate.

## Optional: Cloudflare browser access

Cloudflare Access adds a *public browser endpoint* on top of an already working
Roost installation. It exists for one case: reaching your fleet from a device you
cannot or will not install a VPN client on — a work laptop, a borrowed machine, a
locked-down phone.

What changes and what does not:

| | Automatic Tailscale path | Cloudflare browser access |
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
- the RPCs `AuthRedeemWorker`, `AuthMintCoordinatorRelocation`,
  `AuthRedeemCoordinatorRelocation`, `CoordinatorMovePreflight`,
  `CoordinatorMoveStart`, `CoordinatorMoveStatus`, and `MiscDbExportUrl`

So worker enrollment, worker transport, database export, internal handoff, and
coordinator relocation stay on the coordinator's main HTTPS endpoint whether or
not the public browser endpoint exists.

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

## Other private overlays

WireGuard, Headscale, ZeroTier, and other private overlays can provide the
reachability that automatic Tailscale mode normally supplies. Roost does not
configure or exercise those overlays; you own their routing and DNS. Use a
browser-trusted certificate at the coordinator origin.

No network membership grants Roost authority. Browsers and workers still redeem
scoped one-shot grants or complete explicit pairing, exactly as they do over
Tailscale or direct HTTPS.

## Next

- [Install](/docs/install/) — choose automatic Tailscale or explicit HTTPS
- [Fleet](/docs/fleet/) — why workers only ever dial outbound
- [Security](/docs/security/) — pairing, device keys, and revocation
- [The CLI](/docs/cli/) — `expose`, `status`, `doctor`

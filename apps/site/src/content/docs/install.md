---
title: "Install Roost"
description: "Install the verified Roost release on macOS or Linux — binary installer, Homebrew, the front door you choose, and release assets."
order: 1
section: "Start"
---

## What needs a supported host OS

Only coordinator and worker machines need a supported host OS. In `v0.5.0`,
those roles run on macOS arm64/x64 and Linux arm64/x64. Everything you *browse
from* — a Mac, a Windows PC, a Linux desktop, an iPhone, an Android phone, an
iPad, an Android tablet — needs nothing but a modern browser.

## Start locally

The coordinator binds loopback and speaks plaintext. Install Roost locally
first; no domain, HTTPS proxy, VPN, or external URL is required.

```sh
roost quickstart
```

That creates persistent services with:

```text
ROOST_COORDINATOR_BIND=127.0.0.1:4103
ROOST_TRUST_PROXY=0
ROOST_WEB_PUBLIC_URL=
ROOST_COORDINATOR_PUBLIC_URL=
ROOST_CORS_ALLOWED_ORIGINS=http://127.0.0.1:4103
ROOST_SKIP_ENV_LOCAL=1
```

Quickstart deploys the first local worker and opens a paired browser at
`http://127.0.0.1:4103`. The local worker dials that loopback listener.

## Add a front door when you need remote access

Choose an HTTPS address supplied by the operator's proxy, tunnel, or
private-access setup. Before exposing the running local listener, run this on
the coordinator machine:

```sh
roost quickstart --coordinator-url https://roost.example.com
```

This promotion changes the coordinator endpoint profile and restarts only the
coordinator; it preserves the local worker, keeper, PTYs, and state. Then
configure your front door to forward to the installed loopback bind and
**overwrite** `X-Forwarded-For`. Roost owns no TLS, DNS, tunnel, VPN, SSH, or
target reachability.

**Caddy with your own domain** — Caddy obtains and renews the certificate:

```caddy
roost.example.com {
	@private path /internal/* /api/db-export
	respond @private "not found" 404

	reverse_proxy 127.0.0.1:4103 {
		header_up X-Forwarded-For {remote_host}
	}
}
```

**Cloudflare tunnel** — no open ports, works behind NAT, no certificate on the
box. Point `cloudflared` ingress at a local proxy like the one above;
`cloudflared` appends rather than replaces `X-Forwarded-For`, so it must not be
the last hop.

**`tailscale serve`** — no purchased domain required:

```sh
tailscale serve --bg --https=443 http://127.0.0.1:4103
tailscale serve status
```

The public origin is then the host's MagicDNS name. Full copy-paste recipes,
including the caller-address rule and the private paths, are in
[networking](/docs/networking/). A valid external origin does not prove a
target machine can reach it.

## macOS and Linux

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install-binary.sh | bash
```

Then start the local installation:

```sh
"$HOME/.local/bin/roost" quickstart
```

The installer resolves `uname -s` / `uname -m` to one release asset —
`Darwin/arm64` to `roost`, `Darwin/x86_64` to `roost-darwin-x64`,
`Linux/x86_64` to `roost-linux-x64`, `Linux/aarch64` to `roost-linux-arm64` —
downloads that asset plus its `.sha256` sidecar from the latest release, and
refuses to install on a digest mismatch or a malformed checksum file. The
verified binary is moved into `$HOME/.local/bin/roost` with mode 0755. Override
the destination with `ROOST_BIN_DIR`; the script warns if that directory is not
on your `PATH`. Any other
OS/architecture pair exits with an error rather than guessing an asset.

## Homebrew (macOS)

```sh
brew install cefege/tap/roost
```

The formula is macOS-only on purpose: the unsuffixed `roost` asset is the
darwin-arm64 build and there is no tested Linuxbrew bottle, so Linux installs go
through `install-binary.sh` instead.

## Windows hosts

> **Windows host support is paused for `v0.5.0`.** The release publishes no
> Windows coordinator, worker, package, installer, join script, or updater
> payload. There is no supported Windows host install, enrollment, or update
> procedure in this release. Windows remains supported as a browser client.

`v0.3.2` was the last release with Windows host artifacts. Those historical
artifacts are not an install path for a `v0.5.0` fleet.

## Release assets

Every release publishes a `.sha256` sidecar beside each asset. The unsuffixed
`roost` asset is byte-identical to `roost-darwin-arm64`; it exists so older
release links keep working.

| Asset | Host |
|---|---|
| `roost` | macOS arm64 (compatibility name) |
| `roost-darwin-arm64` | macOS arm64 |
| `roost-darwin-x64` | macOS x64 |
| `roost-linux-x64` | Linux x64 |
| `roost-linux-arm64` | Linux arm64 |

Historical releases through `v0.3.2` also carried Windows packages and signed
PowerShell bootstrap scripts. The Windows release tier is paused, so none of
those artifacts is published or supported for `v0.5.0`.

## Source checkout (development only)

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install.sh | bash
```

This installs Bun and a checkout that tracks `main`, on macOS or Linux. It is a
development path, not the pinned production release path — use
`install-binary.sh` above for anything you intend to keep running.

## Keeping it updated

`roost update` self-updates the published macOS or Linux binary from the latest
GitHub release. `v0.5.0` has no Windows updater payload, so an old Windows host
cannot receive the current release. To update a whole supported fleet in one
command, see [fleet](/docs/fleet/).

## Next

- [Quickstart](/docs/quickstart/) — first coordinator, first phone, first workspace
- [Networking](/docs/networking/) — the loopback listener and your front door
- [The CLI](/docs/cli/) — every subcommand
- [Security](/docs/security/) — pairing, keys, audit, backups

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

## One coordinator shape

The coordinator binds loopback and speaks plaintext; you put a front door in
front of it and tell it the resulting origin:

```sh
roost quickstart --coordinator-url https://roost.example.com
```

That installs `ROOST_COORDINATOR_BIND=127.0.0.1:4103`, `ROOST_TRUST_PROXY=1`,
and `ROOST_WEB_PUBLIC_URL=https://roost.example.com`. Roost owns no TLS, no
DNS, and no tunnel. Pick one front door:

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

**`tailscale serve`** — no domain at all:

```sh
tailscale serve --bg --https=443 http://127.0.0.1:4103
```

The public origin is then the host's MagicDNS name. Full copy-paste recipes,
including the caller-address rule and the private paths, are in
[networking](/docs/networking/).

## macOS and Linux

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install-binary.sh | bash
```

Then run quickstart with the origin your front door serves:

```sh
"$HOME/.local/bin/roost" quickstart --coordinator-url "https://roost.example.com"
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

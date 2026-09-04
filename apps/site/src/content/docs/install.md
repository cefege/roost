---
title: "Install Roost"
description: "Install the verified Roost release on macOS or Linux — binary installer, Homebrew, HTTPS modes, and release assets."
order: 1
section: "Start"
---

## What needs a supported host OS

Only coordinator and worker machines need a supported host OS. In `v0.5.0`,
those roles run on macOS arm64/x64 and Linux arm64/x64. Everything you *browse
from* — a Mac, a Windows PC, a Linux desktop, an iPhone, an Android phone, an
iPad, an Android tablet — needs nothing but a modern browser.

## Choose a coordinator HTTPS mode

`roost quickstart` supports two production topologies:

- **Automatic Tailscale Serve.** With no endpoint flags, Tailscale supplies
  private reachability and browser-trusted HTTPS in front of the coordinator's
  loopback listener.
- **Direct HTTPS.** Supply `--coordinator-url`, `--tls-cert`, and `--tls-key`
  together. The coordinator serves HTTPS itself with your browser-trusted
  certificate; initial coordinator setup and its local worker do not require
  Tailscale. You own DNS, routing, firewall policy, and certificate renewal.

For automatic mode, install and start Tailscale on the macOS or Linux host. On
macOS, use `brew install tailscale` or the Mac App Store. On Linux, follow
[tailscale.com/download/linux](https://tailscale.com/download/linux), enable
`tailscaled`, run `tailscale up`, and set the current user as operator. Approve
the macOS network extension only when the GUI app prompts; the Homebrew daemon
does not use it.

The current extra-worker installer still requires a running Tailscale daemon,
even when that worker dials a direct HTTPS coordinator. That installer
limitation does not make Tailscale a prerequisite for direct coordinator
quickstart. See [networking](/docs/networking/) for both endpoint contracts.

## macOS and Linux

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install-binary.sh | bash
```

Then choose one quickstart form. Automatic Tailscale Serve:

```sh
"$HOME/.local/bin/roost" quickstart
```

Direct HTTPS:

```sh
"$HOME/.local/bin/roost" quickstart \
  --coordinator-url "https://roost.example.com:8443" \
  --tls-cert "$HOME/.config/roost/tls/fullchain.pem" \
  --tls-key "$HOME/.config/roost/tls/privkey.pem"
```

The installer resolves `uname -s` / `uname -m` to one release asset —
`Darwin/arm64` to `roost`, `Darwin/x86_64` to `roost-darwin-x64`,
`Linux/x86_64` to `roost-linux-x64`, `Linux/aarch64` to `roost-linux-arm64` —
downloads that asset plus its `.sha256` sidecar from the latest release, and
refuses to install on a digest mismatch or a malformed checksum file. The
verified binary is moved into `$HOME/.local/bin/roost` with mode 0755. Override
the destination with `ROOST_BIN_DIR`; the script warns if that directory is not
on your `PATH`, and warns separately if Tailscale is missing. Any other
OS/architecture pair exits with an error rather than guessing an asset.

## Homebrew (macOS)

```sh
brew install cefege/tap/roost
```

For automatic mode, start the Homebrew daemon with
`sudo tailscaled install-system-daemon && sudo tailscale up`, then run
`roost quickstart`. For direct HTTPS, run `roost quickstart` with all three
endpoint flags shown above; direct quickstart does not call Tailscale.

The formula is macOS-only on purpose: the unsuffixed `roost` asset is the
darwin-arm64 build and there is no tested Linuxbrew bottle, so Linux installs go
through `install-binary.sh` instead. The formula depends on `tailscale`, which
installs the open-source `tailscaled` — that daemon needs no System Settings
network-extension approval.

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
- [Networking](/docs/networking/) — automatic Tailscale Serve and direct HTTPS
- [The CLI](/docs/cli/) — every subcommand
- [Security](/docs/security/) — pairing, keys, audit, backups

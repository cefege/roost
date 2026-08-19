---
title: "Install Roost"
description: "Install the verified Roost release on macOS, Linux, or Windows x64 — binary installer, Homebrew, the signed PowerShell bootstrap, release assets."
order: 1
section: "Start"
---

## What needs a supported host OS

Only coordinator and worker machines. Those run on macOS, Linux, and Windows x64:
POSIX hosts are supervised by launchd or `systemd --user`, Windows by restricted
SCM services. Everything you *browse from* — a Mac, a Windows PC, a Linux
desktop, an iPhone, an Android phone, an iPad, an Android tablet — needs nothing
but a modern browser.

## Prerequisite: Tailscale

The supported automated production topology requires Tailscale on the
coordinator and on every worker. It is both the private transport and the
trusted enrollment boundary, and it is what lets a phone connect without port
forwarding.

1. Install it: `brew install tailscale` on macOS (or the Mac App Store),
   [tailscale.com/download/linux](https://tailscale.com/download/linux) on
   Linux, [tailscale.com/download/windows](https://tailscale.com/download/windows)
   on Windows.
2. Start it: `tailscale up` — on Linux run `sudo systemctl enable --now tailscaled`
   first.
3. On macOS, approve the Tailscale **network extension** when System Settings
   prompts. That step cannot be automated.

Other networks are covered in [networking](/docs/networking/), including which
parts are manual and unexercised.

## macOS and Linux

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install-binary.sh | bash
"$HOME/.local/bin/roost" quickstart
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
sudo tailscaled install-system-daemon && sudo tailscale up
roost quickstart
```

The formula is macOS-only on purpose: the unsuffixed `roost` asset is the
darwin-arm64 build and there is no tested Linuxbrew bottle, so Linux installs go
through `install-binary.sh` instead. The formula depends on `tailscale`, which
installs the open-source `tailscaled` — that daemon needs no System Settings
network-extension approval.

## Windows x64

> **Windows releases are paused.** `v0.4.2` and later publish macOS and Linux
> assets only: the Windows CI/release tier is disabled while its gates are
> repaired, so no `roost-windows-x64.zip`, manifest, or signed `join.ps1` /
> `install-binary.ps1` exists on `releases/latest`. The procedure below is
> correct and unchanged, but it cannot complete until Windows assets are
> published again. `v0.3.2` is the last release that carries them.

Install Tailscale first. Then open an **elevated PowerShell 5.1+** session and
supply the trusted SHA-256 fingerprint of the release-publisher leaf certificate
through a channel independent of the downloaded release manifest:

```powershell
$publisher = "<trusted publisher certificate SHA-256>".ToLowerInvariant()
if ($publisher -notmatch '^[0-9a-f]{64}$') { throw 'Invalid publisher SHA-256' }
$release = Invoke-RestMethod 'https://api.github.com/repos/cefege/roost/releases/latest'
$base = "https://github.com/cefege/roost/releases/download/$($release.tag_name)"
$programData = [IO.Path]::GetFullPath($env:ProgramData)
if (((Get-Item -LiteralPath $programData -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
  throw 'Unsafe ProgramData directory'
}
$owner = ([Security.Principal.NTAccount](Get-Acl -LiteralPath $programData).Owner).Translate([Security.Principal.SecurityIdentifier]).Value
if ($owner -notin @('S-1-5-18', 'S-1-5-32-544')) { throw 'ProgramData owner is not trusted' }
$staging = Join-Path $programData ('RoostBootstrap-' + [Guid]::NewGuid().ToString('N'))
$security = [Security.AccessControl.DirectorySecurity]::new()
$security.SetSecurityDescriptorSddlForm('O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)')
[IO.Directory]::CreateDirectory($staging, $security) | Out-Null
& icacls.exe $staging '/setintegritylevel' '(OI)(CI)H' | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Cannot apply high-integrity staging policy' }
$installer = Join-Path $staging 'install-binary.ps1'
try {
  Invoke-WebRequest -UseBasicParsing "$base/install-binary.ps1" -OutFile $installer
  $signature = Get-AuthenticodeSignature -LiteralPath $installer
  if ($signature.Status -ne 'Valid' -or $null -eq $signature.SignerCertificate -or $null -eq $signature.TimeStamperCertificate) {
    throw 'Roost installer has no valid Authenticode signature and trusted timestamp'
  }
  $sha256 = [Security.Cryptography.SHA256]::Create()
  try { $actual = -join ($sha256.ComputeHash($signature.SignerCertificate.RawData) | ForEach-Object { $_.ToString('x2') }) }
  finally { $sha256.Dispose() }
  if ($actual -cne $publisher) { throw "Roost installer publisher mismatch: $actual" }
  Set-ExecutionPolicy -Scope Process -ExecutionPolicy RemoteSigned -Force
  & $installer -HostRole coordinator -PublisherSha256 $publisher -ReleaseBaseUrl $base
} finally {
  Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue
}
```

Nothing downloaded is executed before its Authenticode chain, trusted
timestamp, and exact leaf-certificate pin have been verified against the
fingerprint you supplied out of band. The installer then verifies the detached
release manifest signature, the ZIP digest, every per-file digest, the
Authenticode publisher, and the trusted timestamps before installing the
coordinator, worker, keeper, and updater as Windows SCM services.

Services run under a dedicated low-privilege local `roost-operator` identity.
The installer creates that account with a cryptographically random password when
it is absent, rejects administrator identities outright, denies interactive
logon, and prompts for a password only when reusing an existing account.

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
| `roost-windows-x64.zip` | Windows x64 — **not published since `v0.3.2`** |

When the Windows tier is enabled, the Windows package additionally ships
`roost-windows-x64.manifest.json`, its `.sha256`, and a detached `.p7s`
signature, plus the signed `join.ps1` and `install-binary.ps1` bootstrap
scripts, and the release workflow refuses to publish unless every Windows
signing artifact passes. That tier is currently disabled, so a release either
carries the full signed Windows set or — as with `v0.4.2` — no Windows asset at
all; a partial Windows package is never published.

## Source checkout (development only)

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install.sh | bash
```

This installs Bun and a checkout that tracks `main`, on macOS or Linux. It is a
development path, not the pinned production release path — use
`install-binary.sh` above for anything you intend to keep running.

## Keeping it updated

`roost update` self-updates the binary from the latest GitHub release. On an
installed Windows host it queues the signed release through the restricted
updater service with no UAC prompt, then exits so the service can replace the
stable launcher; the updater persists every phase and completes or rolls back on
its own. To update a whole registered fleet in one command, see
[fleet](/docs/fleet/).

## Next

- [Quickstart](/docs/quickstart/) — first coordinator, first phone, first workspace
- [Networking](/docs/networking/) — Tailscale, and the optional Cloudflare browser path
- [The CLI](/docs/cli/) — every subcommand
- [Security](/docs/security/) — pairing, keys, audit, backups

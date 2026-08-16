#requires -Version 7.2
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^https://')]
    [string] $CoordinatorUrl,

    [Security.SecureString] $BootstrapToken,

    [string] $PublisherSha256 = $env:ROOST_WINDOWS_PUBLISHER_SHA256,

    [string] $ServiceAccount = $(
        if ($env:USERDOMAIN) { "$($env:USERDOMAIN)\$($env:USERNAME)" }
        else { $env:USERNAME }
    ),

    [Security.SecureString] $ServiceAccountPassword,

    [string] $InstallRoot = (Join-Path $env:LOCALAPPDATA 'Roost'),

    [string] $ReleaseBaseUrl = 'https://github.com/cefege/roost/releases/latest/download',

    [string] $InstallerPath = (Join-Path $PSScriptRoot 'install-binary.ps1')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Normalize-Sha256([string] $Value) {
    return ($Value -replace '[^0-9A-Fa-f]', '').ToLowerInvariant()
}

function Assert-SignedInstaller([string] $Path, [string] $ExpectedPublisher) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "install-binary.ps1 not found at $Path; download the signed installer beside join.ps1 or pass -InstallerPath"
    }
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.Status -ne [Management.Automation.SignatureStatus]::Valid -or
        $null -eq $signature.SignerCertificate) {
        throw "install-binary.ps1 has no valid Authenticode signature ($($signature.Status))"
    }
    $actual = [Convert]::ToHexString(
        [Security.Cryptography.SHA256]::HashData($signature.SignerCertificate.RawData)
    ).ToLowerInvariant()
    if ($actual -cne $ExpectedPublisher) {
        throw "install-binary.ps1 publisher mismatch: expected $ExpectedPublisher, got $actual"
    }
}

if (-not $IsWindows) { throw 'join.ps1 is Windows-only' }
if ([Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne [Runtime.InteropServices.Architecture]::X64) {
    throw 'the signed Roost Windows release currently supports x64 Windows only'
}
$publisher = Normalize-Sha256 $PublisherSha256
if ($publisher -notmatch '^[0-9a-f]{64}$') {
    throw 'ROOST_WINDOWS_PUBLISHER_SHA256 must be the pinned 64-hex signing leaf certificate SHA-256'
}
if (-not (Get-Command tailscale.exe -ErrorAction SilentlyContinue)) {
    throw 'Tailscale is required. Install it from https://tailscale.com/download/windows and connect before joining Roost.'
}
$tailscale = (& tailscale.exe status --json | Out-String) | ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or $tailscale.BackendState -cne 'Running') {
    throw 'Tailscale must be connected before joining Roost'
}
if (-not $BootstrapToken) {
    $BootstrapToken = Read-Host -AsSecureString 'One-shot Roost worker bootstrap token'
}
if (-not $ServiceAccountPassword) {
    $credential = Get-Credential -UserName $ServiceAccount -Message 'Credential for the restricted Roost service account'
    $ServiceAccount = $credential.UserName
    $ServiceAccountPassword = $credential.Password
}
Assert-SignedInstaller $InstallerPath $publisher

$tokenBstr = [IntPtr]::Zero
$oldCoordinatorUrl = $env:ROOST_COORDINATOR_URL
$oldBootstrapToken = $env:ROOST_BOOTSTRAP_TOKEN
$oldPublisher = $env:ROOST_WINDOWS_PUBLISHER_SHA256
try {
    $tokenBstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($BootstrapToken)
    $token = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($tokenBstr)
    if ([string]::IsNullOrWhiteSpace($token)) { throw 'bootstrap token must not be empty' }

    # These values exist only in this installer process tree. The strict worker
    # enrollment consumes the token before any SCM definition is written; the
    # token is never placed in a service ImagePath or durable file.
    $env:ROOST_COORDINATOR_URL = $CoordinatorUrl
    $env:ROOST_BOOTSTRAP_TOKEN = $token
    $env:ROOST_WINDOWS_PUBLISHER_SHA256 = $publisher

    & $InstallerPath -HostRole worker -PublisherSha256 $publisher `
        -ServiceAccount $ServiceAccount -ServiceAccountPassword $ServiceAccountPassword `
        -InstallRoot $InstallRoot -ReleaseBaseUrl $ReleaseBaseUrl
} finally {
    $env:ROOST_COORDINATOR_URL = $oldCoordinatorUrl
    $env:ROOST_BOOTSTRAP_TOKEN = $oldBootstrapToken
    $env:ROOST_WINDOWS_PUBLISHER_SHA256 = $oldPublisher
    if ($tokenBstr -ne [IntPtr]::Zero) {
        [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($tokenBstr)
    }
    $token = $null
}

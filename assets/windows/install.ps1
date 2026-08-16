#requires -Version 7.2
[CmdletBinding()]
param(
    [ValidateSet('coordinator', 'worker')]
    [string] $HostRole = 'coordinator',

    [ValidateNotNullOrEmpty()]
    [string] $InstallRoot = (Join-Path $env:LOCALAPPDATA 'Roost'),

    [ValidateNotNullOrEmpty()]
    [string] $ServiceDir = $(if ([string]::IsNullOrWhiteSpace($env:ROOST_SERVICE_DIR)) {
        Join-Path $env:LOCALAPPDATA 'Roost\service'
    } else {
        $env:ROOST_SERVICE_DIR
    }),

    [string] $PublisherSha256 = $env:ROOST_WINDOWS_PUBLISHER_SHA256,

    [string] $ServiceAccount = $env:ROOST_SERVICE_ACCOUNT,

    [Security.SecureString] $ServiceAccountPassword,

    [switch] $StageOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Normalize-Sha256([string] $Value) {
    return ($Value -replace '[^0-9A-Fa-f]', '').ToLowerInvariant()
}

function Get-CertificateSha256([Security.Cryptography.X509Certificates.X509Certificate2] $Certificate) {
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        return -join ($sha.ComputeHash($Certificate.RawData) | ForEach-Object { $_.ToString('x2') })
    } finally {
        $sha.Dispose()
    }
}

function Get-Sha256([string] $Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Assert-PublisherSignature([string] $Path, [string] $ExpectedSha256) {
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
        throw "Authenticode signature is not valid for $Path ($($signature.Status))"
    }
    if ($null -eq $signature.SignerCertificate) {
        throw "Authenticode signer is missing for $Path"
    }
    if ((Get-CertificateSha256 $signature.SignerCertificate) -ne $ExpectedSha256) {
        throw "unexpected Authenticode publisher for $Path"
    }
    if ($null -eq $signature.TimeStamperCertificate) {
        throw "trusted timestamp is missing for $Path"
    }
}

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw 'The Roost Windows package can only be installed on Windows.'
}

$expectedPublisher = Normalize-Sha256 $PublisherSha256
if ($expectedPublisher -notmatch '^[0-9a-f]{64}$') {
    throw 'ROOST_WINDOWS_PUBLISHER_SHA256 must pin the lowercase 64-hex SHA-256 of the publisher leaf certificate DER.'
}

if (-not $StageOnly) {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'Run install.ps1 from an elevated PowerShell session.'
    }
    if ([string]::IsNullOrWhiteSpace($ServiceAccount)) {
        throw 'ROOST_SERVICE_ACCOUNT is required.'
    }
    if ($null -eq $ServiceAccountPassword) {
        $ServiceAccountPassword = Read-Host 'Service account password' -AsSecureString
    }
}

$packageRoot = $PSScriptRoot
$checksumPath = Join-Path $packageRoot 'SHA256SUMS'
if (-not (Test-Path -LiteralPath $checksumPath -PathType Leaf)) {
    throw 'SHA256SUMS is missing from the package.'
}

$checksums = [ordered]@{}
foreach ($line in [IO.File]::ReadAllLines($checksumPath, [Text.Encoding]::UTF8)) {
    if ([string]::IsNullOrWhiteSpace($line)) { continue }
    if ($line -notmatch '^([0-9a-f]{64})  ([A-Za-z0-9][A-Za-z0-9._-]*)$') {
        throw "invalid SHA256SUMS entry: $line"
    }
    $name = $Matches[2]
    if ($checksums.Contains($name)) { throw "duplicate SHA256SUMS entry: $name" }
    $checksums[$name] = $Matches[1]
}

$required = @(
    'roost.exe',
    'roost-win-helper.exe',
    'shawl.exe',
    'install.ps1',
    'provision-service-account.ps1',
    'service-templates.json',
    'LICENSE',
    'SHAWL-LICENSE',
    'SHAWL-THIRD-PARTY-LICENSES.txt',
    'shawl-v1.9.0.provenance.json'
)
if ($checksums.Count -ne $required.Count) {
    throw "SHA256SUMS must describe exactly $($required.Count) payload files; found $($checksums.Count)."
}
foreach ($name in $required) {
    if (-not $checksums.Contains($name)) { throw "SHA256SUMS is missing $name" }
    $path = Join-Path $packageRoot $name
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "$name is missing from the package" }
    if ((Get-Sha256 $path) -ne $checksums[$name]) { throw "SHA-256 mismatch for $name" }
}

foreach ($name in @('roost.exe', 'roost-win-helper.exe', 'shawl.exe', 'install.ps1', 'provision-service-account.ps1')) {
    Assert-PublisherSignature (Join-Path $packageRoot $name) $expectedPublisher
}

$packageVersion = (& (Join-Path $packageRoot 'roost.exe') version | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or $packageVersion -notmatch '^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$') {
    throw "roost.exe returned an invalid version: $packageVersion"
}

$versionsRoot = Join-Path $InstallRoot 'versions'
$versionDir = Join-Path $versionsRoot $packageVersion
New-Item -ItemType Directory -Path $versionsRoot -Force | Out-Null

if (Test-Path -LiteralPath $versionDir) {
    foreach ($name in $required) {
        $installedPath = Join-Path $versionDir $name
        if (-not (Test-Path -LiteralPath $installedPath -PathType Leaf) -or
            (Get-Sha256 $installedPath) -ne $checksums[$name]) {
            throw "version $packageVersion is already installed with different content; refusing to overwrite a possibly running executable"
        }
    }
    $installedSums = Join-Path $versionDir 'SHA256SUMS'
    if (-not (Test-Path -LiteralPath $installedSums -PathType Leaf) -or
        (Get-Sha256 $installedSums) -ne (Get-Sha256 $checksumPath)) {
        throw "version $packageVersion has a mismatched SHA256SUMS"
    }
} else {
    $stagingDir = Join-Path $versionsRoot ('.staging-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $stagingDir | Out-Null
    try {
        foreach ($name in $required) {
            [IO.File]::Copy((Join-Path $packageRoot $name), (Join-Path $stagingDir $name), $false)
        }
        [IO.File]::Copy($checksumPath, (Join-Path $stagingDir 'SHA256SUMS'), $false)
        # The destination never exists. Move the new directory atomically; never
        # rename or copy over a running roost.exe.
        [IO.Directory]::Move($stagingDir, $versionDir)
    } finally {
        if (Test-Path -LiteralPath $stagingDir) {
            Remove-Item -LiteralPath $stagingDir -Recurse -Force
        }
    }
}

if ($StageOnly) {
    @{ ok = $true; version = $packageVersion; installDir = $versionDir; started = $false } | ConvertTo-Json -Compress
    return
}
New-Item -ItemType Directory -Path $ServiceDir -Force | Out-Null


$aclArgs = @(
    $versionDir,
    '/inheritance:r',
    '/grant:r',
    "${ServiceAccount}:(OI)(CI)RX",
    '*S-1-5-18:(OI)(CI)F',
    '*S-1-5-32-544:(OI)(CI)F'
)
& icacls.exe @aclArgs | Out-Null
if ($LASTEXITCODE -ne 0) { throw "failed to protect the version directory ACL (exit $LASTEXITCODE)" }
$serviceAclArgs = @(
    $ServiceDir,
    '/inheritance:r',
    '/grant:r',
    "${ServiceAccount}:(OI)(CI)M",
    '*S-1-5-18:(OI)(CI)F',
    '*S-1-5-32-544:(OI)(CI)F'
)
& icacls.exe @serviceAclArgs | Out-Null
if ($LASTEXITCODE -ne 0) { throw "failed to protect the service directory ACL (exit $LASTEXITCODE)" }


$installedRoost = Join-Path $versionDir 'roost.exe'
$roostCommand = if ($HostRole -eq 'coordinator') { 'quickstart' } else { 'join' }
$startInfo = [Diagnostics.ProcessStartInfo]::new()
$startInfo.FileName = $installedRoost
$startInfo.UseShellExecute = $false
$startInfo.RedirectStandardInput = $true
$startInfo.ArgumentList.Add($roostCommand)
$startInfo.ArgumentList.Add('--windows-service-credential-stdin')
$startInfo.Environment['ROOST_WINDOWS_PUBLISHER_SHA256'] = $expectedPublisher
$startInfo.Environment['ROOST_SERVICE_ACCOUNT'] = $ServiceAccount
$startInfo.Environment['ROOST_SERVICE_DIR'] = $ServiceDir
$null = $startInfo.Environment.Remove('ROOST_SERVICE_ACCOUNT_PASSWORD')
$null = $startInfo.Environment.Remove('ROOST_SERVICE_PASSWORD')

$passwordBstr = [IntPtr]::Zero
$passwordBytes = $null
$lengthBytes = $null
$child = $null
try {
    $passwordBstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($ServiceAccountPassword)
    $plainPassword = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($passwordBstr)
    $passwordBytes = [Text.Encoding]::UTF8.GetBytes($plainPassword)
    $plainPassword = $null
    if ($passwordBytes.Length -lt 1 -or $passwordBytes.Length -gt 16384) {
        throw 'service account password must encode to 1..16384 UTF-8 bytes.'
    }
    $lengthBytes = [BitConverter]::GetBytes([uint32]$passwordBytes.Length)
    if (-not [BitConverter]::IsLittleEndian) { [Array]::Reverse($lengthBytes) }

    $child = [Diagnostics.Process]::Start($startInfo)
    $child.StandardInput.BaseStream.Write($lengthBytes, 0, $lengthBytes.Length)
    $child.StandardInput.BaseStream.Write($passwordBytes, 0, $passwordBytes.Length)
    $child.StandardInput.Close()
    $child.WaitForExit()
    if ($child.ExitCode -ne 0) {
        throw "roost.exe $roostCommand failed with exit code $($child.ExitCode)"
    }
} finally {
    if ($null -ne $child) { $child.Dispose() }
    if ($null -ne $passwordBytes) { [Array]::Clear($passwordBytes, 0, $passwordBytes.Length) }
    if ($null -ne $lengthBytes) { [Array]::Clear($lengthBytes, 0, $lengthBytes.Length) }
    if ($passwordBstr -ne [IntPtr]::Zero) { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($passwordBstr) }
}

@{ ok = $true; version = $packageVersion; installDir = $versionDir; serviceDir = $ServiceDir; started = $true; hostRole = $HostRole } |
    ConvertTo-Json -Compress

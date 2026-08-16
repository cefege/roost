#requires -Version 7.2
[CmdletBinding()]
param(
    [ValidateSet('coordinator', 'worker')]
    [string] $HostRole = 'coordinator',

    [string] $PublisherSha256 = $env:ROOST_WINDOWS_PUBLISHER_SHA256,

    [string] $ServiceAccount = $(
        if ($env:USERDOMAIN) { "$($env:USERDOMAIN)\$($env:USERNAME)" }
        else { $env:USERNAME }
    ),

    [Security.SecureString] $ServiceAccountPassword,

    [string] $InstallRoot = (Join-Path $env:LOCALAPPDATA 'Roost'),

    [string] $ReleaseBaseUrl = 'https://github.com/cefege/roost/releases/latest/download'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Normalize-Sha256([string] $Value) {
    return ($Value -replace '[^0-9A-Fa-f]', '').ToLowerInvariant()
}

function Get-Sha256([string] $Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Assert-HexSha256([string] $Value, [string] $Label) {
    if ((Normalize-Sha256 $Value) -notmatch '^[0-9a-f]{64}$') {
        throw "$Label must be exactly 64 hexadecimal characters"
    }
}

function Download-File([string] $Uri, [string] $Destination) {
    if (-not $Uri.StartsWith('https://', [StringComparison]::OrdinalIgnoreCase)) {
        throw "refusing non-HTTPS release URL: $Uri"
    }
    Invoke-WebRequest -UseBasicParsing -Uri $Uri -OutFile $Destination
}

function Assert-DetachedCms(
    [byte[]] $Content,
    [byte[]] $Signature,
    [string] $ExpectedPublisher
) {
    Add-Type -AssemblyName System.Security.Cryptography.Pkcs
    $contentInfo = [Security.Cryptography.Pkcs.ContentInfo]::new(, $Content)
    $cms = [Security.Cryptography.Pkcs.SignedCms]::new($contentInfo, $true)
    $cms.Decode($Signature)
    $cms.CheckSignature($false)
    if ($cms.SignerInfos.Count -ne 1) {
        throw "release manifest must have exactly one CMS signer; found $($cms.SignerInfos.Count)"
    }
    $certificate = $cms.SignerInfos[0].Certificate
    if ($null -eq $certificate) { throw 'release manifest CMS signer has no certificate' }
    $actual = [Convert]::ToHexString(
        [Security.Cryptography.SHA256]::HashData($certificate.RawData)
    ).ToLowerInvariant()
    if ($actual -cne $ExpectedPublisher) {
        throw "release manifest publisher mismatch: expected $ExpectedPublisher, got $actual"
    }
}

function Assert-Authenticode([string] $Path, [string] $ExpectedPublisher) {
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.Status -ne [Management.Automation.SignatureStatus]::Valid) {
        throw "invalid Authenticode signature for $Path ($($signature.Status): $($signature.StatusMessage))"
    }
    if ($null -eq $signature.SignerCertificate) {
        throw "missing Authenticode signer certificate for $Path"
    }
    $actual = [Convert]::ToHexString(
        [Security.Cryptography.SHA256]::HashData($signature.SignerCertificate.RawData)
    ).ToLowerInvariant()
    if ($actual -cne $ExpectedPublisher) {
        throw "Authenticode publisher mismatch for $Path: expected $ExpectedPublisher, got $actual"
    }
}

function Protect-OperatorPath([string] $Path, [string] $Account, [switch] $Container) {
    $operatorRights = if ($Container) { "${Account}:(OI)(CI)F" } else { "${Account}:F" }
    $arguments = @(
        $Path,
        '/inheritance:r',
        '/grant:r',
        $operatorRights,
        $(if ($Container) { '*S-1-5-18:(OI)(CI)F' } else { '*S-1-5-18:F' }),
        $(if ($Container) { '*S-1-5-32-544:(OI)(CI)F' } else { '*S-1-5-32-544:F' })
    )
    & icacls.exe @arguments | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "failed to protect ACL on $Path (exit $LASTEXITCODE)" }
}

if (-not $IsWindows) { throw 'install-binary.ps1 is Windows-only' }
if ([Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne [Runtime.InteropServices.Architecture]::X64) {
    throw 'the signed Roost Windows release currently supports x64 Windows only'
}
if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) { throw 'LOCALAPPDATA is required' }
if ([string]::IsNullOrWhiteSpace($ServiceAccount)) { throw 'an explicit operator service account is required' }
if ($ServiceAccount -match '^(?i:LocalSystem|SYSTEM|NT AUTHORITY\\SYSTEM|\.\\LocalSystem)$') {
    throw 'LocalSystem is forbidden; choose the interactive Roost operator account'
}
if (-not (Get-Command tailscale.exe -ErrorAction SilentlyContinue)) {
    throw 'Tailscale is required. Install it from https://tailscale.com/download/windows and connect before installing Roost.'
}
if (-not $ServiceAccountPassword) {
    $credential = Get-Credential -UserName $ServiceAccount -Message 'Credential for the restricted Roost service account'
    $ServiceAccount = $credential.UserName
    $ServiceAccountPassword = $credential.Password
}

$publisher = Normalize-Sha256 $PublisherSha256
Assert-HexSha256 $publisher 'ROOST_WINDOWS_PUBLISHER_SHA256'

$manifestName = 'roost-windows-x64.manifest.json'
$signatureName = 'roost-windows-x64.manifest.json.p7s'
$packageName = 'roost-windows-x64.zip'
$manifestUrl = "$($ReleaseBaseUrl.TrimEnd('/'))/$manifestName"
$signatureUrl = "$($ReleaseBaseUrl.TrimEnd('/'))/$signatureName"
$packageUrl = "$($ReleaseBaseUrl.TrimEnd('/'))/$packageName"
$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ('roost-install-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tempRoot | Out-Null

try {
    $manifestPath = Join-Path $tempRoot $manifestName
    $signaturePath = Join-Path $tempRoot $signatureName
    $packagePath = Join-Path $tempRoot $packageName
    Download-File $manifestUrl $manifestPath
    Download-File $signatureUrl $signaturePath

    $manifestBytes = [IO.File]::ReadAllBytes($manifestPath)
    $signatureBytes = [IO.File]::ReadAllBytes($signaturePath)
    Assert-DetachedCms $manifestBytes $signatureBytes $publisher
    $manifestSha256 = Get-Sha256 $manifestPath
    $manifest = [Text.Encoding]::UTF8.GetString($manifestBytes) | ConvertFrom-Json -Depth 32

    if ($manifest.schemaVersion -ne 1 -or $manifest.platform -cne 'win32' -or $manifest.arch -cne 'x64') {
        throw 'unsupported Windows release manifest identity'
    }
    if ($manifest.package.name -cne $packageName) { throw 'manifest package name is not canonical' }
    Assert-HexSha256 ([string] $manifest.package.sha256) 'manifest package SHA-256'
    if ([long] $manifest.package.size -le 0) { throw 'manifest package size is invalid' }
    if ($manifest.shawl.version -cne '1.9.0') { throw 'manifest Shawl version must be 1.9.0' }
    Assert-HexSha256 ([string] $manifest.shawl.upstreamSha256) 'manifest Shawl upstream SHA-256'
    if ([string]::IsNullOrWhiteSpace([string] $manifest.version)) { throw 'manifest version is missing' }
    if ([string]::IsNullOrWhiteSpace([string] $manifest.publishedAt)) { throw 'manifest publishedAt is missing' }

    Download-File $packageUrl $packagePath
    $packageInfo = Get-Item -LiteralPath $packagePath
    if ($packageInfo.Length -ne [long] $manifest.package.size) {
        throw "release ZIP size mismatch: expected $($manifest.package.size), got $($packageInfo.Length)"
    }
    $actualPackageSha = Get-Sha256 $packagePath
    if ($actualPackageSha -cne (Normalize-Sha256 ([string] $manifest.package.sha256))) {
        throw "release ZIP checksum mismatch: expected $($manifest.package.sha256), got $actualPackageSha"
    }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::OpenRead($packagePath)
    try {
        $entries = @($archive.Entries | Where-Object { -not [string]::IsNullOrEmpty($_.Name) })
        $manifestFiles = @($manifest.files)
        if ($entries.Count -ne $manifestFiles.Count) {
            throw "ZIP file count does not match signed manifest ($($entries.Count) vs $($manifestFiles.Count))"
        }
        $allowed = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        foreach ($file in $manifestFiles) {
            $relative = ([string] $file.path).Replace('\\', '/')
            if ([string]::IsNullOrWhiteSpace($relative) -or
                [IO.Path]::IsPathRooted($relative) -or
                $relative.Split('/') -contains '..') {
                throw "unsafe path in signed manifest: $relative"
            }
            if (-not $allowed.Add($relative)) { throw "duplicate path in signed manifest: $relative" }
            Assert-HexSha256 ([string] $file.sha256) "manifest file SHA-256 ($relative)"
            if ([long] $file.size -lt 0) { throw "invalid manifest file size: $relative" }
        }
        foreach ($entry in $entries) {
            $relative = $entry.FullName.Replace('\\', '/')
            if (-not $allowed.Contains($relative)) { throw "unsigned file in release ZIP: $relative" }
        }
    } finally {
        $archive.Dispose()
    }

    $packageRoot = Join-Path $tempRoot 'package'
    [IO.Compression.ZipFile]::ExtractToDirectory($packagePath, $packageRoot)
    foreach ($file in @($manifest.files)) {
        $relative = ([string] $file.path).Replace('/', [IO.Path]::DirectorySeparatorChar)
        $path = Join-Path $packageRoot $relative
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "release file is missing: $relative" }
        $info = Get-Item -LiteralPath $path
        if ($info.Length -ne [long] $file.size) { throw "release file size mismatch: $relative" }
        if ((Get-Sha256 $path) -cne (Normalize-Sha256 ([string] $file.sha256))) {
            throw "release file checksum mismatch: $relative"
        }
        if ([bool] $file.authenticodeRequired) { Assert-Authenticode $path $publisher }
    }

    $packageInstaller = Join-Path $packageRoot 'install.ps1'
    if (-not (Test-Path -LiteralPath $packageInstaller -PathType Leaf)) {
        throw 'signed release ZIP does not contain install.ps1'
    }
    Assert-Authenticode $packageInstaller $publisher

    $serviceDir = Join-Path $InstallRoot 'service'
    $versionsDir = Join-Path $InstallRoot 'versions'
    New-Item -ItemType Directory -Path $serviceDir -Force | Out-Null
    New-Item -ItemType Directory -Path $versionsDir -Force | Out-Null
    Protect-OperatorPath $InstallRoot $ServiceAccount -Container

    # Stage first so the protected current manifest exists before updater/SCM
    # services can be installed or started.
    & $packageInstaller -HostRole $HostRole -InstallRoot $InstallRoot `
        -PublisherSha256 $publisher -ServiceAccount $ServiceAccount `
        -ServiceAccountPassword $ServiceAccountPassword -StageOnly | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "signed package staging failed (exit $LASTEXITCODE)" }

    $versionDir = Join-Path $versionsDir ([string] $manifest.version)
    if (-not (Test-Path -LiteralPath $versionDir -PathType Container)) {
        throw "signed package did not stage expected version directory: $versionDir"
    }
    $current = [ordered]@{
        schemaVersion = 1
        version = [string] $manifest.version
        versionDir = $versionDir
        files = @($manifest.files | ForEach-Object {
            [ordered]@{ path = [string] $_.path; sha256 = (Normalize-Sha256 ([string] $_.sha256)); size = [long] $_.size }
        })
        manifestUrl = $manifestUrl
        signatureUrl = $signatureUrl
        manifestSha256 = $manifestSha256
        publisherSha256 = $publisher
    }
    $currentPath = Join-Path $serviceDir 'current.json'
    $currentTemp = Join-Path $serviceDir ('.current-' + [Guid]::NewGuid().ToString('N') + '.tmp')
    [IO.File]::WriteAllText(
        $currentTemp,
        (($current | ConvertTo-Json -Depth 16) + "`n"),
        [Text.UTF8Encoding]::new($false)
    )
    Move-Item -LiteralPath $currentTemp -Destination $currentPath -Force
    Protect-OperatorPath $currentPath $ServiceAccount

    $oldServiceDir = $env:ROOST_SERVICE_DIR
    $oldPublisher = $env:ROOST_WINDOWS_PUBLISHER_SHA256
    try {
        $env:ROOST_SERVICE_DIR = $serviceDir
        $env:ROOST_WINDOWS_PUBLISHER_SHA256 = $publisher
        & $packageInstaller -HostRole $HostRole -InstallRoot $InstallRoot `
            -PublisherSha256 $publisher -ServiceAccount $ServiceAccount `
            -ServiceAccountPassword $ServiceAccountPassword
        if ($LASTEXITCODE -ne 0) { throw "signed package install failed (exit $LASTEXITCODE)" }
    } finally {
        $env:ROOST_SERVICE_DIR = $oldServiceDir
        $env:ROOST_WINDOWS_PUBLISHER_SHA256 = $oldPublisher
    }
} finally {
    if (Test-Path -LiteralPath $tempRoot) {
        Remove-Item -LiteralPath $tempRoot -Recurse -Force
    }
}

#requires -Version 5.1
[CmdletBinding()]
param(
    [ValidateSet('coordinator', 'worker')]
    [string] $HostRole = 'coordinator',

    [ValidateNotNullOrEmpty()]
    [string] $InstallRoot = (Join-Path $env:ProgramData 'Roost'),

    [ValidateNotNullOrEmpty()]
    [string] $ServiceDir = $(if ([string]::IsNullOrWhiteSpace($env:ROOST_SERVICE_DIR)) {
        Join-Path $env:ProgramData 'Roost\service'
    } else {
        $env:ROOST_SERVICE_DIR
    }),

    [string] $PublisherSha256 = $env:ROOST_WINDOWS_PUBLISHER_SHA256,

    [string] $ServiceAccount = $env:ROOST_SERVICE_ACCOUNT,

    [Security.SecureString] $ServiceAccountPassword,

    [string] $ExpectedVersion,
    [string] $ExpectedBuild,
    [switch] $StageOnly,

    [string] $CoordinatorUrl,

    [string] $TlsCert,

    [string] $TlsKey
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Assert-DirectCoordinatorUrl([string] $Value) {
    if ([string]::IsNullOrWhiteSpace($Value) -or $Value -match '\p{Cc}') {
        throw 'CoordinatorUrl must be a non-empty HTTPS origin without control characters'
    }
    if ($Value -match '^https://(?<authority>[^/?#]+)/?$') {
        $authority = [string] $Matches['authority']
    } else {
        throw 'CoordinatorUrl must be an HTTPS origin with no userinfo, path, query, or fragment'
    }
    $uri = $null
    if (-not [Uri]::TryCreate($Value, [UriKind]::Absolute, [ref] $uri) -or
        $uri.Scheme -ine [Uri]::UriSchemeHttps -or
        [string]::IsNullOrWhiteSpace($uri.Host) -or
        -not [string]::IsNullOrEmpty($uri.UserInfo) -or
        $uri.AbsolutePath -cne '/' -or
        -not [string]::IsNullOrEmpty($uri.Query) -or
        -not [string]::IsNullOrEmpty($uri.Fragment)) {
        throw 'CoordinatorUrl must be an HTTPS origin with no userinfo, path, query, or fragment'
    }
    if ($authority.StartsWith('[', [StringComparison]::Ordinal)) {
        if ($authority -match '^\[[^\]]+\]:(?<port>[0-9]+)$') {
            $portText = [string] $Matches['port']
        } else {
            throw 'CoordinatorUrl must include an explicit numeric port'
        }
    } elseif ($authority -match '^[^:]+:(?<port>[0-9]+)$') {
        $portText = [string] $Matches['port']
    } else {
        throw 'CoordinatorUrl must include an explicit numeric port'
    }
    $port = 0
    if (-not [int]::TryParse($portText, [ref] $port) -or
        $port -lt 1 -or $port -gt 65535) {
        throw 'CoordinatorUrl port must be between 1 and 65535'
    }
}

function Get-NormalizedDirectTlsPath([string] $Value, [string] $Label) {
    if ([string]::IsNullOrWhiteSpace($Value) -or $Value -match '\p{Cc}') {
        throw "$Label must be a non-empty rooted path without control characters"
    }
    try {
        if (-not [IO.Path]::IsPathRooted($Value)) {
            throw [ArgumentException]::new()
        }
        $normalized = [IO.Path]::GetFullPath($Value)
    } catch {
        throw "$Label must be a lexically valid rooted path"
    }
    return $normalized.TrimEnd([char[]] @([char] 92, [char] 47))
}

$directTlsParameterCount = 0
foreach ($name in @('CoordinatorUrl', 'TlsCert', 'TlsKey')) {
    if ($PSBoundParameters.ContainsKey($name)) {
        $directTlsParameterCount += 1
    }
}
if ($directTlsParameterCount -ne 0 -and $directTlsParameterCount -ne 3) {
    throw 'CoordinatorUrl, TlsCert, and TlsKey must be supplied together'
}
$directTlsMode = $directTlsParameterCount -eq 3
if ($directTlsMode) {
    if ($HostRole -ne 'coordinator') {
        throw 'CoordinatorUrl, TlsCert, and TlsKey are only valid for a coordinator install'
    }
    Assert-DirectCoordinatorUrl $CoordinatorUrl
    $normalizedTlsCert = Get-NormalizedDirectTlsPath $TlsCert 'TlsCert'
    $normalizedTlsKey = Get-NormalizedDirectTlsPath $TlsKey 'TlsKey'
    if ($normalizedTlsCert.Equals(
        $normalizedTlsKey,
        [StringComparison]::OrdinalIgnoreCase
    )) {
        throw 'TlsCert and TlsKey must identify distinct paths'
    }
    foreach ($entry in @(
        [pscustomobject]@{ Path = $normalizedTlsCert; Label = 'TlsCert' },
        [pscustomobject]@{ Path = $normalizedTlsKey; Label = 'TlsKey' }
    )) {
        $path = [string] $entry.Path
        $label = [string] $entry.Label
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "$label must identify a readable regular file"
        }
        $item = Get-Item -LiteralPath $path -Force
        $linkType = $item.PSObject.Properties['LinkType']
        if ($item.PSIsContainer -or
            ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
            ($null -ne $linkType -and
                -not [string]::IsNullOrWhiteSpace([string] $linkType.Value))) {
            throw "$label must identify a non-link regular file"
        }
        $stream = $null
        try {
            $stream = [IO.File]::Open(
                $path,
                [IO.FileMode]::Open,
                [IO.FileAccess]::Read,
                [IO.FileShare]::ReadWrite
            )
        } catch {
            throw "$label must identify a readable regular file"
        } finally {
            if ($null -ne $stream) { $stream.Dispose() }
        }
    }
}
$icaclsPath = Join-Path ([Environment]::SystemDirectory) 'icacls.exe'
if (-not (Test-Path -LiteralPath $icaclsPath -PathType Leaf)) {
    throw 'trusted Windows icacls.exe is missing'
}

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

function Assert-RegularNonReparseFile([string] $Path, [string] $Label) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Label is missing: $Path"
    }
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer -or
        ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "$Label must be a regular non-reparse file: $Path"
    }
    $linkType = $item.PSObject.Properties['LinkType']
    if ($null -ne $linkType -and
        -not [string]::IsNullOrWhiteSpace([string] $linkType.Value)) {
        throw "$Label must not be a hard link or symbolic link: $Path"
    }
}

function Assert-NonReparseDirectory([string] $Path, [string] $Label) {
    if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
        throw "$Label is missing: $Path"
    }
    $item = Get-Item -LiteralPath $Path -Force
    if (-not $item.PSIsContainer -or
        ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "$Label must be a non-reparse directory: $Path"
    }
}

function Invoke-VerifiedHelper([string] $Helper, [string[]] $Arguments) {
    Assert-RegularNonReparseFile $Helper 'signed Windows helper'
    $output = (& $Helper @Arguments | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) {
        throw "Windows helper operation '$($Arguments[0])' failed (exit $LASTEXITCODE)"
    }
    return $output
}


function Protect-ExactVersionFile(
    [string] $Path,
    [string] $Helper,
    [switch] $RequireExact
) {
    Assert-RegularNonReparseFile $Path 'version payload file'
    if ($RequireExact) {
        $proof = Invoke-VerifiedHelper $Helper @('inspect-updater-artifact', $Path, 'release')
        try { $parsed = $proof | ConvertFrom-Json }
        catch { throw "Windows helper returned invalid release artifact proof: $proof" }
        if ([string] $parsed.profile -cne 'release') {
            throw "Windows helper returned the wrong release artifact profile: $Path"
        }
        return
    }
    $proof = Invoke-VerifiedHelper $Helper @('protect-updater-artifact', $Path, 'release')
    try { $parsed = $proof | ConvertFrom-Json }
    catch { throw "Windows helper returned invalid release protection proof: $proof" }
    if ($parsed.protected -ne $true) {
        throw "Windows helper did not protect the release artifact: $Path"
    }
}

function Publish-ProtectedStableMetadata(
    [string] $Path,
    [string] $Contents,
    [Text.Encoding] $Encoding,
    [string] $Helper
) {
    $parent = [IO.Path]::GetDirectoryName($Path)
    $staged = Join-Path $parent ('.bootstrap-' + [Guid]::NewGuid().ToString('N'))
    try {
        [IO.File]::WriteAllText($staged, $Contents + "`n", $Encoding)
        Assert-RegularNonReparseFile $staged 'stable metadata staging file'
        $proof = Invoke-VerifiedHelper $Helper @(
            'protect-updater-artifact', $staged, 'release'
        )
        try { $parsed = $proof | ConvertFrom-Json }
        catch { throw "Windows helper returned invalid stable metadata protection proof: $proof" }
        if ($parsed.protected -ne $true) {
            throw "Windows helper did not protect stable metadata: $Path"
        }
        $null = Invoke-VerifiedHelper $Helper @('flush-file', $staged)
        $null = Invoke-VerifiedHelper $Helper @('replace-file', $staged, $Path)
        $null = Invoke-VerifiedHelper $Helper @('flush-file', $Path)
        $null = Invoke-VerifiedHelper $Helper @('flush-file', $parent)
        $null = Invoke-VerifiedHelper $Helper @('inspect-updater-artifact', $Path, 'release')
    } finally {
        if (Test-Path -LiteralPath $staged) {
            Remove-Item -LiteralPath $staged -Force
        }
    }
}

function Assert-CanonicalProductionLayout([string] $Root, [string] $ConfiguredServiceDir) {
    if (-not [IO.Path]::IsPathRooted($Root)) { throw 'InstallRoot must be an absolute path' }
    $root = [IO.Path]::GetFullPath($Root).TrimEnd('\')
    $programData = [IO.Path]::GetFullPath(
        [Environment]::GetFolderPath([Environment+SpecialFolder]::CommonApplicationData)
    ).TrimEnd('\')
    $expectedRoot = [IO.Path]::GetFullPath((Join-Path $programData 'Roost')).TrimEnd('\')
    if (-not $root.Equals($expectedRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'custom Windows install roots are not supported for service installation'
    }
    $expectedService = [IO.Path]::GetFullPath((Join-Path $root 'service')).TrimEnd('\')
    $actualService = [IO.Path]::GetFullPath($ConfiguredServiceDir).TrimEnd('\')
    if (-not $actualService.Equals($expectedService, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'ServiceDir must be the canonical service directory beneath InstallRoot'
    }
}

function Get-TrustedTailscaleExecutable() {
    $programFiles = [Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFiles)
    if ([string]::IsNullOrWhiteSpace($programFiles)) {
        throw 'the trusted machine-wide Program Files location is unavailable'
    }
    $path = [IO.Path]::GetFullPath((Join-Path $programFiles 'Tailscale\tailscale.exe'))
    Assert-RegularNonReparseFile $path 'machine-wide Tailscale executable'
    $cursor = Split-Path -Parent $path
    while (-not [string]::IsNullOrWhiteSpace($cursor)) {
        $item = Get-Item -LiteralPath $cursor -Force
        if (-not $item.PSIsContainer -or
            ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "machine-wide Tailscale path contains an unsafe component: $cursor"
        }
        if ($cursor.TrimEnd('\').Equals(
            [IO.Path]::GetFullPath($programFiles).TrimEnd('\'),
            [StringComparison]::OrdinalIgnoreCase
        )) { break }
        $parent = [IO.Directory]::GetParent($cursor)
        if ($null -eq $parent) { throw 'machine-wide Tailscale escaped Program Files' }
        $cursor = $parent.FullName
    }
    $signature = Get-AuthenticodeSignature -LiteralPath $path
    if ($signature.Status -ne [Management.Automation.SignatureStatus]::Valid -or
        $null -eq $signature.SignerCertificate -or
        $null -eq $signature.TimeStamperCertificate) {
        throw 'the machine-wide Tailscale executable lacks a valid timestamped Authenticode signature'
    }
    return $path
}

function ConvertTo-WindowsCommandLineArgument([string] $Value) {
    if ($null -eq $Value) {
        throw 'process arguments must not be null'
    }
    $quoted = [Text.StringBuilder]::new()
    $null = $quoted.Append([char] 34)
    $backslashes = 0
    foreach ($character in $Value.ToCharArray()) {
        if ($character -eq [char] 92) {
            $backslashes += 1
            continue
        }
        if ($character -eq [char] 34) {
            if ($backslashes -gt 0) {
                $null = $quoted.Append([char] 92, $backslashes * 2)
            }
            $null = $quoted.Append([char] 92)
            $null = $quoted.Append([char] 34)
        } else {
            if ($backslashes -gt 0) {
                $null = $quoted.Append([char] 92, $backslashes)
            }
            $null = $quoted.Append($character)
        }
        $backslashes = 0
    }
    if ($backslashes -gt 0) {
        $null = $quoted.Append([char] 92, $backslashes * 2)
    }
    $null = $quoted.Append([char] 34)
    return $quoted.ToString()
}


if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw 'The Roost Windows package can only be installed on Windows.'
}

$expectedPublisher = Normalize-Sha256 $PublisherSha256
if ($expectedPublisher -notmatch '^[0-9a-f]{64}$') {
    throw 'ROOST_WINDOWS_PUBLISHER_SHA256 must pin the lowercase 64-hex SHA-256 of the publisher leaf certificate DER.'
}
$env:ROOST_WINDOWS_PUBLISHER_SHA256 = $expectedPublisher


if (-not [IO.Path]::IsPathRooted($ServiceDir)) {
    throw 'ServiceDir must be an absolute path'
}
$ServiceDir = [IO.Path]::GetFullPath($ServiceDir)

$interactiveSid = if ([string]::IsNullOrWhiteSpace($env:ROOST_INTERACTIVE_SID)) {
    [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
} else {
    $env:ROOST_INTERACTIVE_SID
}
if ($interactiveSid -notmatch '^S-1-[0-9-]+$') { throw 'ROOST_INTERACTIVE_SID is invalid' }
$env:ROOST_INTERACTIVE_SID = $interactiveSid


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
if ($LASTEXITCODE -ne 0 -or $packageVersion -notmatch '^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$') {
    throw "roost.exe returned an invalid version: $packageVersion"
}
$releaseVersion = ($packageVersion -split '\+', 2)[0]
if (-not [string]::IsNullOrWhiteSpace($ExpectedVersion) -and $releaseVersion -cne $ExpectedVersion) {
    throw "roost.exe release version $releaseVersion does not match signed manifest version $ExpectedVersion"
}
$packageBuild = (& (Join-Path $packageRoot 'roost.exe') version --build | Out-String).Trim().ToLowerInvariant()
if ($LASTEXITCODE -ne 0 -or $packageBuild -notmatch '^[0-9a-f]{40,64}$') {
    throw "roost.exe returned an invalid immutable build identity: $packageBuild"
}
if (-not [string]::IsNullOrWhiteSpace($ExpectedBuild) -and $packageBuild -cne $ExpectedBuild) {
    throw "roost.exe build $packageBuild does not match signed manifest build $ExpectedBuild"
}

if ($StageOnly -and [string]::IsNullOrWhiteSpace($ServiceAccount)) {
    # Release-package verification runs StageOnly without an installation
    # identity. It has already checked every checksum/signature and executes no
    # privileged filesystem or SCM mutation.
    @{ ok = $true; verifiedOnly = $true; version = $releaseVersion; buildVersion = $packageVersion; started = $false } |
        ConvertTo-Json -Compress
    return
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run install.ps1 from an elevated PowerShell session.'
}
if ([string]::IsNullOrWhiteSpace($ServiceAccount)) {
    throw 'ROOST_SERVICE_ACCOUNT is required.'
}
if ([string]::IsNullOrWhiteSpace($ExpectedVersion) -or
    [string]::IsNullOrWhiteSpace($ExpectedBuild)) {
    throw 'service installation requires the signed manifest version and build identity'
}
if (-not $StageOnly -and $null -eq $ServiceAccountPassword) {
    $ServiceAccountPassword = Read-Host 'Service account password' -AsSecureString
}
Assert-CanonicalProductionLayout $InstallRoot $ServiceDir
$InstallRoot = [IO.Path]::GetFullPath($InstallRoot)
$ServiceDir = [IO.Path]::GetFullPath($ServiceDir)
$bootstrapHelper = Join-Path $packageRoot 'roost-win-helper.exe'
$versionsRoot = Join-Path $InstallRoot 'versions'
$stableBin = Join-Path $InstallRoot 'bin'
$protectedDirectories = @(
    @($InstallRoot, 'install-root'),
    @($versionsRoot, 'versions-root'),
    @($ServiceDir, 'service-root'),
    @($stableBin, 'stable-bin')
)
foreach ($entry in $protectedDirectories) {
    $path = [string] $entry[0]
    $profile = [string] $entry[1]
    if (-not (Test-Path -LiteralPath $path)) {
        [IO.Directory]::CreateDirectory($path) | Out-Null
    }
    Assert-NonReparseDirectory $path "protected $profile"
    $null = Invoke-VerifiedHelper $bootstrapHelper @(
        'protect-directory', $path, $profile, $ServiceAccount, $interactiveSid
    )
}

$versionDir = Join-Path $versionsRoot $releaseVersion
$versionFiles = @($required + 'SHA256SUMS')

if (Test-Path -LiteralPath $versionDir) {
    Assert-NonReparseDirectory $versionDir 'pre-existing version directory'
    $entries = @(Get-ChildItem -LiteralPath $versionDir -Force)
    if ($entries.Count -ne $versionFiles.Count) {
        throw "version $packageVersion contains an unexpected file set"
    }
    $null = Invoke-VerifiedHelper $bootstrapHelper @(
        'protect-directory', $versionDir, 'versions-bootstrap', $ServiceAccount, $interactiveSid
    )
    foreach ($name in $versionFiles) {
        $installedPath = Join-Path $versionDir $name
        Protect-ExactVersionFile $installedPath $bootstrapHelper -RequireExact
        $expectedHash = if ($name -ceq 'SHA256SUMS') {
            Get-Sha256 $checksumPath
        } else {
            $checksums[$name]
        }
        if ((Get-Sha256 $installedPath) -cne $expectedHash) {
            throw "version $packageVersion is already installed with different content"
        }
    }
} else {
    $stagingDir = Join-Path $versionsRoot ('.staging-' + [Guid]::NewGuid().ToString('N'))
    [IO.Directory]::CreateDirectory($stagingDir) | Out-Null
    try {
        $null = Invoke-VerifiedHelper $bootstrapHelper @(
            'protect-directory', $stagingDir, 'versions-bootstrap', $ServiceAccount, $interactiveSid
        )
        foreach ($name in $required) {
            $source = Join-Path $packageRoot $name
            $destination = Join-Path $stagingDir $name
            [IO.File]::Copy($source, $destination, $false)
            if ((Get-Sha256 $destination) -cne $checksums[$name]) {
                throw "staged version file checksum mismatch: $name"
            }
            Protect-ExactVersionFile $destination $bootstrapHelper
            $null = Invoke-VerifiedHelper $bootstrapHelper @('flush-file', $destination)
        }
        $stagedSums = Join-Path $stagingDir 'SHA256SUMS'
        [IO.File]::Copy($checksumPath, $stagedSums, $false)
        if ((Get-Sha256 $stagedSums) -cne (Get-Sha256 $checksumPath)) {
            throw 'staged SHA256SUMS checksum mismatch'
        }
        Protect-ExactVersionFile $stagedSums $bootstrapHelper
        $null = Invoke-VerifiedHelper $bootstrapHelper @('flush-file', $stagedSums)
        # Directory.Move is an atomic no-replace publication.
        [IO.Directory]::Move($stagingDir, $versionDir)
        $null = Invoke-VerifiedHelper $bootstrapHelper @(
            'protect-directory', $versionDir, 'versions-bootstrap', $ServiceAccount, $interactiveSid
        )
    } finally {
        if (Test-Path -LiteralPath $stagingDir) {
            Remove-Item -LiteralPath $stagingDir -Recurse -Force
        }
    }
}
if ($StageOnly) {
    @{ ok = $true; version = $releaseVersion; buildVersion = $packageVersion; installDir = $versionDir; started = $false } |
        ConvertTo-Json -Compress
    return
}
$stableShawl = Join-Path $stableBin 'shawl.exe'
$stableLauncher = Join-Path $stableBin 'roost.exe'
$installRootMetadata = Join-Path $stableBin 'install-root.txt'
$publisherMetadata = Join-Path $stableBin 'publisher.sha256'
$stableExecutables = @($stableShawl, $stableLauncher)
$existingStableExecutables = @($stableExecutables | Where-Object { Test-Path -LiteralPath $_ })
if ($existingStableExecutables.Count -eq 0) {
    $sourceShawl = Join-Path $versionDir 'shawl.exe'
    $sourceLauncher = Join-Path $versionDir 'roost-win-helper.exe'
    $null = Invoke-VerifiedHelper $bootstrapHelper @(
        'copy-updater-artifact', $sourceShawl, $stableShawl,
        'release', 'stable-shawl', $checksums['shawl.exe'],
        [string] ([IO.FileInfo] $sourceShawl).Length
    )
    $null = Invoke-VerifiedHelper $bootstrapHelper @(
        'copy-updater-artifact', $sourceLauncher, $stableLauncher,
        'release', 'stable-launcher', $checksums['roost-win-helper.exe'],
        [string] ([IO.FileInfo] $sourceLauncher).Length
    )
} elseif ($existingStableExecutables.Count -ne $stableExecutables.Count) {
    throw 'stable executable bootstrap is incomplete; refusing partial state'
}
if (-not (Test-Path -LiteralPath $installRootMetadata)) {
    Publish-ProtectedStableMetadata `
        $installRootMetadata $InstallRoot ([Text.UTF8Encoding]::new($false)) `
        $bootstrapHelper
}
if (-not (Test-Path -LiteralPath $publisherMetadata)) {
    Publish-ProtectedStableMetadata `
        $publisherMetadata $expectedPublisher ([Text.ASCIIEncoding]::new()) `
        $bootstrapHelper
}
foreach ($entry in @(
    @($stableShawl, 'stable-shawl', $checksums['shawl.exe']),
    @($stableLauncher, 'stable-launcher', $checksums['roost-win-helper.exe'])
)) {
    $path = [string] $entry[0]
    Assert-RegularNonReparseFile $path 'stable-layout executable'
    $null = Invoke-VerifiedHelper $bootstrapHelper @(
        'inspect-updater-artifact', $path, [string] $entry[1],
        [string] $entry[2], [string] ([IO.FileInfo] $path).Length
    )
}
foreach ($path in @($installRootMetadata, $publisherMetadata)) {
    Assert-RegularNonReparseFile $path 'stable-layout metadata'
    $null = Invoke-VerifiedHelper $bootstrapHelper @(
        'inspect-updater-artifact', $path, 'release'
    )
}
if ((Get-Sha256 $stableShawl) -cne $checksums['shawl.exe'] -or
    (Get-Sha256 $stableLauncher) -cne $checksums['roost-win-helper.exe']) {
    throw 'stable-layout executables differ from the verified package bytes'
}
Assert-PublisherSignature $stableShawl $expectedPublisher
Assert-PublisherSignature $stableLauncher $expectedPublisher
$configuredRoot = [IO.File]::ReadAllText($installRootMetadata, [Text.Encoding]::UTF8).Trim()
$configuredPublisher = [IO.File]::ReadAllText($publisherMetadata, [Text.Encoding]::ASCII).Trim()
if (-not ([IO.Path]::GetFullPath($configuredRoot)).Equals(
    $InstallRoot,
    [StringComparison]::OrdinalIgnoreCase
) -or $configuredPublisher -cne $expectedPublisher) {
    throw 'stable-layout metadata disagrees with the canonical install root or publisher'
}
$activeBuild = (& $stableLauncher version --build | Out-String).Trim().ToLowerInvariant()
if ($LASTEXITCODE -ne 0 -or $activeBuild -cne $ExpectedBuild.ToLowerInvariant()) {
    throw 'stable dispatcher did not resolve the expected authenticated build'
}

$system32 = [IO.Path]::GetFullPath([Environment]::SystemDirectory)
Assert-NonReparseDirectory $system32 'trusted Windows System32'
Assert-RegularNonReparseFile (Join-Path $system32 'sc.exe') 'trusted Windows sc.exe'
$tailscaleExecutable = if ($directTlsMode) {
    $null
} else {
    Get-TrustedTailscaleExecutable
}
$serviceHome = Join-Path $ServiceDir 'home'
if (-not (Test-Path -LiteralPath $serviceHome)) {
    [IO.Directory]::CreateDirectory($serviceHome) | Out-Null
}
Assert-NonReparseDirectory $serviceHome 'canonical Windows service home'
$null = Invoke-VerifiedHelper $bootstrapHelper @(
    'protect-directory', $serviceHome, 'service-home', $ServiceAccount, $interactiveSid
)
$serviceLocalAppData = Join-Path $serviceHome 'AppData\Local'
$serviceRoamingAppData = Join-Path $serviceHome 'AppData\Roaming'
$serviceTemp = Join-Path $serviceLocalAppData 'Temp'
foreach ($path in @($serviceLocalAppData, $serviceRoamingAppData, $serviceTemp)) {
    [IO.Directory]::CreateDirectory($path) | Out-Null
}

$dataRoot = Join-Path $ServiceDir 'data'
$logRoot = Join-Path $ServiceDir 'logs'
$keeperData = Join-Path $dataRoot 'keeper'
$workerData = Join-Path $dataRoot 'worker'
$coordinatorData = Join-Path $dataRoot 'coordinator'
$updaterData = Join-Path $dataRoot 'updater'
$keeperLogs = Join-Path $logRoot 'keeper'
$workerLogs = Join-Path $logRoot 'worker'
$coordinatorLogs = Join-Path $logRoot 'coordinator'
$updaterLogs = Join-Path $logRoot 'updater'
$coordinatorTls = Join-Path $coordinatorData 'tls'
$legacyLocalAppData = $env:LOCALAPPDATA
if ([string]::IsNullOrWhiteSpace($legacyLocalAppData) -or
    -not [IO.Path]::IsPathRooted($legacyLocalAppData) -or
    $legacyLocalAppData -match "[`0`r`n]") {
    throw 'the invoking user LOCALAPPDATA is required for fail-closed legacy state migration'
}
$legacyCoordinatorData = [IO.Path]::GetFullPath(
    (Join-Path $legacyLocalAppData 'Roost\CoordinatorV2')
)

$installedRoost = Join-Path $versionDir 'roost.exe'
$installedHelper = Join-Path $versionDir 'roost-win-helper.exe'
Assert-RegularNonReparseFile $installedHelper 'installed same-release Windows helper'
if ((Get-Sha256 $installedHelper) -cne $checksums['roost-win-helper.exe']) {
    throw 'installed same-release Windows helper checksum mismatch'
}
Assert-PublisherSignature $installedHelper $expectedPublisher
Protect-ExactVersionFile $installedHelper $bootstrapHelper -RequireExact
$roostCommand = if ($HostRole -eq 'coordinator') { 'quickstart' } else { 'join' }
$startInfo = [Diagnostics.ProcessStartInfo]::new()
$startInfo.FileName = $installedRoost
$startInfo.UseShellExecute = $false
$startInfo.RedirectStandardInput = $true
$roostArguments = @($roostCommand, '--windows-service-credential-stdin')
if ($directTlsMode) {
    $roostArguments += @(
        '--coordinator-url', $CoordinatorUrl,
        '--tls-cert', $TlsCert,
        '--tls-key', $TlsKey
    )
}
$argumentListProperty = $startInfo.GetType().GetProperty('ArgumentList')
if ($null -ne $argumentListProperty) {
    $argumentList = $argumentListProperty.GetValue($startInfo)
    foreach ($argument in $roostArguments) {
        $null = $argumentList.Add($argument)
    }
} else {
    $quotedArguments = @($roostArguments | ForEach-Object {
        ConvertTo-WindowsCommandLineArgument $_
    })
    $startInfo.Arguments = $quotedArguments -join ' '
}
$canonicalEnvironment = [ordered]@{
    'ROOST_WINDOWS_PUBLISHER_SHA256' = $expectedPublisher
    'ROOST_SERVICE_ACCOUNT' = $ServiceAccount
    'ROOST_INTERACTIVE_SID' = $interactiveSid
    'ROOST_SYSTEM32' = $system32
    'ROOST_INSTALL_ROOT' = $InstallRoot
    'ROOST_SERVICE_DIR' = $ServiceDir
    'ROOST_VERSIONS_DIR' = $versionsRoot
    'ROOST_STABLE_SHAWL_PATH' = $stableShawl
    'ROOST_STABLE_LAUNCHER' = $stableLauncher
    'ROOST_WIN_HELPER' = $installedHelper
    'ROOST_KEEPER_DATA_DIR' = $keeperData
    'ROOST_WORKER_DATA_DIR' = $workerData
    'ROOST_COORD_DATA_DIR' = $coordinatorData
    'ROOST_UPDATER_DATA_DIR' = $updaterData
    'ROOST_KEEPER_LOG_DIR' = $keeperLogs
    'ROOST_WORKER_LOG_DIR' = $workerLogs
    'ROOST_COORD_LOG_DIR' = $coordinatorLogs
    'ROOST_UPDATER_LOG_DIR' = $updaterLogs
    'ROOST_COORDINATOR_DB' = (Join-Path $coordinatorData 'coordinator_v2.db')
    'ROOST_COORDINATOR_AUTHORIZED_KEYS' = (Join-Path $coordinatorData 'authorized_keys.roost')
    'ROOST_COORDINATOR_KEY_PATH' = (Join-Path $coordinatorData 'ssh_ed25519.key')
    'ROOST_COORDINATOR_HANDOFF_PATH' = (Join-Path $coordinatorData 'coord-handoff.json')
    'ROOST_COORDINATOR_TLS_DIR' = $coordinatorTls
    'ROOST_LEGACY_COORD_DATA_DIR' = $legacyCoordinatorData
    'USERPROFILE' = $serviceHome
    'HOME' = $serviceHome
    'APPDATA' = $serviceRoamingAppData
    'LOCALAPPDATA' = $serviceLocalAppData
    'TEMP' = $serviceTemp
    'TMP' = $serviceTemp
}
foreach ($entry in $canonicalEnvironment.GetEnumerator()) {
    $startInfo.EnvironmentVariables[$entry.Key] = [string] $entry.Value
}
if ($directTlsMode) {
    $null = $startInfo.EnvironmentVariables.Remove('ROOST_TAILSCALE_EXE')
    $null = $startInfo.EnvironmentVariables.Remove('ROOST_TAILNET_HTTPS_PORT')
} else {
    $startInfo.EnvironmentVariables['ROOST_TAILSCALE_EXE'] = [string] $tailscaleExecutable
}
$null = $startInfo.EnvironmentVariables.Remove('ROOST_SERVICE_ACCOUNT_PASSWORD')
$null = $startInfo.EnvironmentVariables.Remove('ROOST_SERVICE_PASSWORD')

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

@{ ok = $true; version = $releaseVersion; buildVersion = $packageVersion; installDir = $versionDir; serviceDir = $ServiceDir; started = $true; hostRole = $HostRole } |
    ConvertTo-Json -Compress

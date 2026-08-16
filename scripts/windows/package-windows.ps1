#requires -Version 7.2
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string] $Version,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string] $PublishedAt,

    [string] $RoostExe = 'dist/roost-windows-x64.exe',
    [string] $HelperExe = 'dist/roost-win-helper.exe',
    [string] $OutputDir = 'dist',
    [string] $StageDir = 'dist/windows-package',

    [string] $ShawlWin64Sha256 = $env:SHAWL_WIN64_SHA256,
    [string] $ShawlLegalSha256 = $env:SHAWL_LEGAL_SHA256,

    [switch] $Unsigned
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$packageName = 'roost-windows-x64.zip'
$manifestName = 'roost-windows-x64.manifest.json'
$manifestSignatureName = 'roost-windows-x64.manifest.json.p7s'
$provenancePath = 'assets/windows/shawl-v1.9.0.provenance.json'
$payloadNames = @(
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
$authenticodeNames = @(
    'roost.exe',
    'roost-win-helper.exe',
    'shawl.exe',
    'install.ps1',
    'provision-service-account.ps1'
)

function Normalize-Sha256([string] $Value, [string] $Name) {
    $normalized = ($Value -replace '[^0-9A-Fa-f]', '').ToLowerInvariant()
    if ($normalized -notmatch '^[0-9a-f]{64}$') { throw "$Name must be an explicit 64-hex SHA-256 pin." }
    return $normalized
}

function Get-CertificateSha256([Security.Cryptography.X509Certificates.X509Certificate2] $Certificate) {
    return [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($Certificate.RawData)).ToLowerInvariant()
}

function Get-Sha256([string] $Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Write-Sha256File([string] $Path) {
    [IO.File]::WriteAllText(
        "$Path.sha256",
        (Get-Sha256 $Path) + "`n",
        [Text.UTF8Encoding]::new($false)
    )
}

function Assert-ExitCode([string] $Operation) {
    if ($LASTEXITCODE -ne 0) { throw "$Operation failed with exit code $LASTEXITCODE" }
}

function Assert-Authenticode([string] $Path, [string] $ExpectedSha256) {
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
        throw "Authenticode validation failed for $Path ($($signature.Status))"
    }
    if ($null -eq $signature.SignerCertificate -or
        (Get-CertificateSha256 $signature.SignerCertificate) -ne $ExpectedSha256) {
        throw "unexpected Authenticode publisher for $Path"
    }
    if ($null -eq $signature.TimeStamperCertificate) {
        throw "trusted Authenticode timestamp missing for $Path"
    }
}

function Find-SignTool {
    $roots = @(
        (Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10/bin'),
        (Join-Path $env:ProgramFiles 'Windows Kits/10/bin')
    ) | Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Container) }
    $tools = foreach ($root in $roots) {
        Get-ChildItem -LiteralPath $root -Filter 'signtool.exe' -Recurse -File |
            Where-Object { $_.DirectoryName -match '[\\/]x64$' }
    }
    $selected = $tools | Sort-Object -Property FullName -Descending | Select-Object -First 1
    if ($null -eq $selected) { throw 'Windows SDK x64 signtool.exe is required.' }
    return $selected.FullName
}

function New-DeterministicZip([string] $SourceDir, [string[]] $Names, [string] $Destination, [DateTimeOffset] $Timestamp) {
    Add-Type -AssemblyName System.IO.Compression
    if (Test-Path -LiteralPath $Destination) { Remove-Item -LiteralPath $Destination -Force }
    $stream = [IO.File]::Open($Destination, [IO.FileMode]::CreateNew, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    try {
        $archive = [IO.Compression.ZipArchive]::new($stream, [IO.Compression.ZipArchiveMode]::Create, $true)
        try {
            foreach ($name in ($Names | Sort-Object)) {
                $sourcePath = Join-Path $SourceDir $name
                $entry = $archive.CreateEntry($name, [IO.Compression.CompressionLevel]::Optimal)
                $entry.LastWriteTime = $Timestamp
                $input = [IO.File]::OpenRead($sourcePath)
                $output = $entry.Open()
                try { $input.CopyTo($output) } finally { $output.Dispose(); $input.Dispose() }
            }
        } finally {
            $archive.Dispose()
        }
    } finally {
        $stream.Dispose()
    }
}

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw 'Windows release packages must be assembled and signed on Windows.'
}
if ($Version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?$') {
    throw "Version must be a release semver without a leading v: $Version"
}
try {
    $publishedTimestamp = [DateTimeOffset]::ParseExact(
        $PublishedAt,
        'yyyy-MM-ddTHH:mm:ssZ',
        [Globalization.CultureInfo]::InvariantCulture,
        [Globalization.DateTimeStyles]::AssumeUniversal
    ).ToUniversalTime()
} catch {
    throw "PublishedAt must be canonical UTC (yyyy-MM-ddTHH:mm:ssZ): $PublishedAt"
}
if ($publishedTimestamp.Year -lt 1980) { throw 'PublishedAt predates the ZIP timestamp epoch.' }
if (-not (Test-Path -LiteralPath $RoostExe -PathType Leaf)) { throw "missing Windows binary: $RoostExe" }
if (-not (Test-Path -LiteralPath $HelperExe -PathType Leaf)) { throw "missing Windows helper: $HelperExe" }
if (-not (Test-Path -LiteralPath $provenancePath -PathType Leaf)) { throw "missing Shawl provenance: $provenancePath" }

$provenance = Get-Content -LiteralPath $provenancePath -Raw | ConvertFrom-Json
$expectedWin64 = Normalize-Sha256 $provenance.binary.sha256 'checked-in Shawl win64 SHA-256'
$expectedLegal = Normalize-Sha256 $provenance.legal.sha256 'checked-in Shawl legal SHA-256'
$providedWin64 = Normalize-Sha256 $ShawlWin64Sha256 'SHAWL_WIN64_SHA256'
$providedLegal = Normalize-Sha256 $ShawlLegalSha256 'SHAWL_LEGAL_SHA256'
if ($providedWin64 -ne $expectedWin64) { throw 'SHAWL_WIN64_SHA256 does not match checked-in provenance.' }
if ($providedLegal -ne $expectedLegal) { throw 'SHAWL_LEGAL_SHA256 does not match checked-in provenance.' }

$signingPfxBase64 = $env:WINDOWS_SIGNING_PFX_BASE64
$signingPfxPassword = $env:WINDOWS_SIGNING_PFX_PASSWORD
$expectedPublisher = if ($Unsigned) { '' } else {
    Normalize-Sha256 $env:ROOST_WINDOWS_PUBLISHER_SHA256 'ROOST_WINDOWS_PUBLISHER_SHA256'
}
$timestampUrl = $env:WINDOWS_TIMESTAMP_URL
if (-not $Unsigned) {
    if ([string]::IsNullOrWhiteSpace($signingPfxBase64)) { throw 'WINDOWS_SIGNING_PFX_BASE64 is required for a release package.' }
    if ([string]::IsNullOrWhiteSpace($signingPfxPassword)) { throw 'WINDOWS_SIGNING_PFX_PASSWORD is required for a release package.' }
    if ($expectedPublisher -notmatch '^[0-9a-f]{64}$') { throw 'ROOST_WINDOWS_PUBLISHER_SHA256 must pin the release publisher leaf certificate DER.' }
    if ([string]::IsNullOrWhiteSpace($timestampUrl) -or $timestampUrl -notmatch '^https://') {
        throw 'WINDOWS_TIMESTAMP_URL must pin a trusted HTTPS RFC 3161 timestamp service.'
    }
}

$roostVersion = (& $RoostExe version | Out-String).Trim()
Assert-ExitCode 'roost.exe version'
if ($roostVersion -ne $Version -and -not $roostVersion.StartsWith("$Version+", [StringComparison]::Ordinal)) {
    throw "roost.exe version '$roostVersion' does not match package version '$Version'."
}
$helperVersion = (& $HelperExe version | Out-String).Trim()
Assert-ExitCode 'roost-win-helper.exe version'
try { $null = $helperVersion | ConvertFrom-Json } catch { throw 'roost-win-helper.exe version did not emit protocol/build JSON.' }

$output = [IO.Path]::GetFullPath($OutputDir)
$stage = [IO.Path]::GetFullPath($StageDir)
New-Item -ItemType Directory -Path $output -Force | Out-Null
if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
New-Item -ItemType Directory -Path $stage | Out-Null

$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ('roost-package-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tempRoot | Out-Null
$pfxPath = Join-Path $tempRoot 'publisher.pfx'
$certificate = $null
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $shawlArchive = Join-Path $tempRoot $provenance.binary.name
    $legalArchive = Join-Path $tempRoot $provenance.legal.name
    Invoke-WebRequest -Uri $provenance.binary.url -OutFile $shawlArchive -MaximumRedirection 10
    Invoke-WebRequest -Uri $provenance.legal.url -OutFile $legalArchive -MaximumRedirection 10
    if ((Get-Sha256 $shawlArchive) -ne $expectedWin64) { throw 'downloaded Shawl win64 archive failed its pinned SHA-256.' }
    if ((Get-Sha256 $legalArchive) -ne $expectedLegal) { throw 'downloaded Shawl legal archive failed its pinned SHA-256.' }
    if ((Get-Item -LiteralPath $shawlArchive).Length -ne [long]$provenance.binary.size) { throw 'downloaded Shawl win64 archive has an unexpected size.' }
    if ((Get-Item -LiteralPath $legalArchive).Length -ne [long]$provenance.legal.size) { throw 'downloaded Shawl legal archive has an unexpected size.' }

    $shawlExtract = Join-Path $tempRoot 'shawl'
    $legalExtract = Join-Path $tempRoot 'legal'
    Expand-Archive -LiteralPath $shawlArchive -DestinationPath $shawlExtract
    Expand-Archive -LiteralPath $legalArchive -DestinationPath $legalExtract
    $shawlExecutables = @(Get-ChildItem -LiteralPath $shawlExtract -Filter 'shawl.exe' -Recurse -File)
    $legalFiles = @(Get-ChildItem -LiteralPath $legalExtract -Filter 'shawl-v1.9.0-legal.txt' -Recurse -File)
    if ($shawlExecutables.Count -ne 1) { throw "expected one shawl.exe in the pinned archive; found $($shawlExecutables.Count)." }
    if ($legalFiles.Count -ne 1) { throw "expected one Shawl legal inventory; found $($legalFiles.Count)." }

    [IO.File]::Copy([IO.Path]::GetFullPath($RoostExe), (Join-Path $stage 'roost.exe'), $false)
    [IO.File]::Copy([IO.Path]::GetFullPath($HelperExe), (Join-Path $stage 'roost-win-helper.exe'), $false)
    [IO.File]::Copy($shawlExecutables[0].FullName, (Join-Path $stage 'shawl.exe'), $false)
    [IO.File]::Copy('assets/windows/install.ps1', (Join-Path $stage 'install.ps1'), $false)
    [IO.File]::Copy('assets/windows/provision-service-account.ps1', (Join-Path $stage 'provision-service-account.ps1'), $false)
    [IO.File]::Copy('assets/windows/service-templates.json', (Join-Path $stage 'service-templates.json'), $false)
    [IO.File]::Copy('LICENSE', (Join-Path $stage 'LICENSE'), $false)
    [IO.File]::Copy('assets/windows/SHAWL-LICENSE', (Join-Path $stage 'SHAWL-LICENSE'), $false)
    [IO.File]::Copy($legalFiles[0].FullName, (Join-Path $stage 'SHAWL-THIRD-PARTY-LICENSES.txt'), $false)
    [IO.File]::Copy($provenancePath, (Join-Path $stage 'shawl-v1.9.0.provenance.json'), $false)

    if (-not $Unsigned) {
        try {
            [IO.File]::WriteAllBytes($pfxPath, [Convert]::FromBase64String($signingPfxBase64))
        } catch {
            throw 'WINDOWS_SIGNING_PFX_BASE64 is not valid base64.'
        }
        $securePassword = ConvertTo-SecureString $signingPfxPassword -AsPlainText -Force
        $certificate = [Security.Cryptography.X509Certificates.X509Certificate2]::new(
            $pfxPath,
            $securePassword,
            [Security.Cryptography.X509Certificates.X509KeyStorageFlags]::Exportable -bor
                [Security.Cryptography.X509Certificates.X509KeyStorageFlags]::EphemeralKeySet
        )
        if (-not $certificate.HasPrivateKey) { throw 'the release signing certificate has no private key.' }
        if ((Get-CertificateSha256 $certificate) -ne $expectedPublisher) {
            throw 'release signing certificate does not match ROOST_WINDOWS_PUBLISHER_SHA256.'
        }
        if ([DateTimeOffset]::UtcNow -lt $certificate.NotBefore -or [DateTimeOffset]::UtcNow -gt $certificate.NotAfter) {
            throw 'release signing certificate is not currently valid.'
        }
        $codeSigningOid = '1.3.6.1.5.5.7.3.3'
        $hasCodeSigningEku = $false
        foreach ($extension in $certificate.Extensions) {
            if ($extension -is [Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension]) {
                foreach ($usage in $extension.EnhancedKeyUsages) {
                    if ($usage.Value -eq $codeSigningOid) { $hasCodeSigningEku = $true }
                }
            }
        }
        if (-not $hasCodeSigningEku) { throw 'release signing certificate lacks the Code Signing EKU.' }

        $signTool = Find-SignTool
        foreach ($name in @('roost.exe', 'roost-win-helper.exe', 'shawl.exe')) {
            $signArgs = @(
                'sign', '/fd', 'SHA256', '/f', $pfxPath, '/p', $signingPfxPassword,
                '/tr', $timestampUrl, '/td', 'SHA256', (Join-Path $stage $name)
            )
            & $signTool @signArgs
            Assert-ExitCode "signtool sign $name"
        }
        foreach ($name in @('install.ps1', 'provision-service-account.ps1')) {
            $result = Set-AuthenticodeSignature -LiteralPath (Join-Path $stage $name) -Certificate $certificate `
                -HashAlgorithm SHA256 -TimestampServer $timestampUrl -IncludeChain All
            if ($result.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
                throw "Set-AuthenticodeSignature failed for $name ($($result.Status): $($result.StatusMessage))"
            }
        }
        foreach ($name in $authenticodeNames) {
            Assert-Authenticode (Join-Path $stage $name) $expectedPublisher
        }
    }

    $sumLines = foreach ($name in $payloadNames) {
        "$(Get-Sha256 (Join-Path $stage $name))  $name"
    }
    [IO.File]::WriteAllText(
        (Join-Path $stage 'SHA256SUMS'),
        ($sumLines -join "`n") + "`n",
        [Text.UTF8Encoding]::new($false)
    )

    $archiveNames = @($payloadNames) + @('SHA256SUMS')
    $zipPath = Join-Path $output $packageName
    New-DeterministicZip $stage $archiveNames $zipPath $publishedTimestamp
    Write-Sha256File $zipPath

    $manifestFiles = foreach ($name in $archiveNames) {
        $file = Get-Item -LiteralPath (Join-Path $stage $name)
        [ordered]@{
            path = $name
            sha256 = Get-Sha256 $file.FullName
            size = [long]$file.Length
            authenticodeRequired = $authenticodeNames -contains $name
        }
    }
    $zip = Get-Item -LiteralPath $zipPath
    $manifest = [ordered]@{
        schemaVersion = 1
        version = $Version
        platform = 'win32'
        arch = 'x64'
        publishedAt = $publishedTimestamp.ToString('yyyy-MM-ddTHH:mm:ssZ')
        package = [ordered]@{
            name = $packageName
            sha256 = Get-Sha256 $zip.FullName
            size = [long]$zip.Length
        }
        files = @($manifestFiles)
        shawl = [ordered]@{
            version = '1.9.0'
            upstreamSha256 = $expectedWin64
        }
    }
    $manifestPath = Join-Path $output $manifestName
    [IO.File]::WriteAllText(
        $manifestPath,
        ($manifest | ConvertTo-Json -Depth 6 -Compress) + "`n",
        [Text.UTF8Encoding]::new($false)
    )
    Write-Sha256File $manifestPath

    $manifestSignaturePath = Join-Path $output $manifestSignatureName
    if (-not $Unsigned) {
        $p7Args = @(
            'sign', '/fd', 'SHA256', '/f', $pfxPath, '/p', $signingPfxPassword,
            '/tr', $timestampUrl, '/td', 'SHA256', '/p7', $output,
            '/p7ce', 'DetachedSignedData', $manifestPath
        )
        & $signTool @p7Args
        Assert-ExitCode 'signtool detached manifest signing'
        $generatedP7 = Join-Path $output ($manifestName + '.p7')
        if (-not (Test-Path -LiteralPath $generatedP7 -PathType Leaf)) {
            throw "signtool did not produce the expected detached CMS file: $generatedP7"
        }
        [IO.File]::Copy($generatedP7, $manifestSignaturePath, $true)
        Remove-Item -LiteralPath $generatedP7 -Force

        Add-Type -AssemblyName System.Security.Cryptography.Pkcs
        $cms = [Security.Cryptography.Pkcs.SignedCms]::new(
            [Security.Cryptography.Pkcs.ContentInfo]::new([IO.File]::ReadAllBytes($manifestPath)),
            $true
        )
        $cms.Decode([IO.File]::ReadAllBytes($manifestSignaturePath))
        $cms.CheckSignature($true)
        $cmsSigner = $cms.SignerInfos[0].Certificate
        if ($null -eq $cmsSigner -or (Get-CertificateSha256 $cmsSigner) -ne $expectedPublisher) {
            throw 'detached manifest CMS has an unexpected publisher.'
        }
        Write-Sha256File $manifestSignaturePath
    }

    [ordered]@{
        package = $zipPath
        manifest = $manifestPath
        signature = if ($Unsigned) { $null } else { $manifestSignaturePath }
        version = $Version
        roostVersion = $roostVersion
        unsigned = [bool]$Unsigned
    } | ConvertTo-Json -Compress
} finally {
    if ($null -ne $certificate) { $certificate.Dispose() }
    if (Test-Path -LiteralPath $tempRoot) { Remove-Item -LiteralPath $tempRoot -Recurse -Force }
}

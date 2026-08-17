#requires -Version 7.2
[CmdletBinding()]
param(
    [string] $Package = 'dist/roost-windows-x64.zip',
    [string] $Manifest = 'dist/roost-windows-x64.manifest.json',
    [string] $Signature = 'dist/roost-windows-x64.manifest.json.p7s',
    [string] $PublisherSha256 = $env:ROOST_WINDOWS_PUBLISHER_SHA256,
    [switch] $AllowUnsigned
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

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
    'shawl-v1.9.0.provenance.json',
    'SHA256SUMS'
)
$authenticodeNames = @('roost.exe', 'roost-win-helper.exe', 'shawl.exe', 'install.ps1', 'provision-service-account.ps1')
$bootstrapScriptNames = @('join.ps1', 'install-binary.ps1')

function Get-Sha256([string] $Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Normalize-Sha256([string] $Value) {
    return ($Value -replace '[^0-9A-Fa-f]', '').ToLowerInvariant()
}

function Get-CertificateSha256([Security.Cryptography.X509Certificates.X509Certificate2] $Certificate) {
    return [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($Certificate.RawData)).ToLowerInvariant()
}

function Assert-Sha256Sidecar([string] $Path) {
    $sidecar = "$Path.sha256"
    if (-not (Test-Path -LiteralPath $sidecar -PathType Leaf)) { throw "missing checksum sidecar: $sidecar" }
    $expected = [IO.File]::ReadAllText($sidecar, [Text.Encoding]::UTF8).Trim().ToLowerInvariant()
    if ($expected -notmatch '^[0-9a-f]{64}$' -or $expected -ne (Get-Sha256 $Path)) {
        throw "invalid checksum sidecar: $sidecar"
    }
}

function Assert-Authenticode([string] $Path, [string] $ExpectedSha256) {
    $signatureInfo = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signatureInfo.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
        throw "invalid Authenticode signature for $Path ($($signatureInfo.Status))"
    }
    if ($null -eq $signatureInfo.SignerCertificate -or
        (Get-CertificateSha256 $signatureInfo.SignerCertificate) -ne $ExpectedSha256) {
        throw "unexpected Authenticode publisher for $Path"
    }
    if ($null -eq $signatureInfo.TimeStamperCertificate) { throw "missing trusted timestamp for $Path" }
}

foreach ($path in @($Package, $Manifest)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "missing release asset: $path" }
    Assert-Sha256Sidecar $path
}
$releaseDir = Split-Path -Parent ([IO.Path]::GetFullPath($Package))
foreach ($name in $bootstrapScriptNames) {
    $path = Join-Path $releaseDir $name
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "missing release bootstrap asset: $path" }
    Assert-Sha256Sidecar $path
}
$expectedPublisher = if ($AllowUnsigned) { '' } else { Normalize-Sha256 $PublisherSha256 }
if (-not $AllowUnsigned) {
    if ($expectedPublisher -notmatch '^[0-9a-f]{64}$') {
        throw 'ROOST_WINDOWS_PUBLISHER_SHA256 must pin the expected publisher leaf certificate DER.'
    }
    if (-not (Test-Path -LiteralPath $Signature -PathType Leaf)) { throw "missing manifest signature: $Signature" }
    Assert-Sha256Sidecar $Signature
}

$manifestObject = Get-Content -LiteralPath $Manifest -Raw | ConvertFrom-Json
$topLevelKeys = @($manifestObject.PSObject.Properties.Name | Sort-Object)
$expectedTopLevelKeys = @('arch', 'build', 'files', 'package', 'platform', 'publishedAt', 'schemaVersion', 'shawl', 'version') | Sort-Object
if (($topLevelKeys -join ',') -ne ($expectedTopLevelKeys -join ',')) { throw 'manifest has an unexpected top-level schema.' }
if ($manifestObject.schemaVersion -ne 1 -or $manifestObject.platform -ne 'win32' -or $manifestObject.arch -ne 'x64') {
    throw 'manifest platform/schema invariants failed.'
}
if ($manifestObject.version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?$') { throw 'manifest version is invalid.' }
if ($manifestObject.build -notmatch '^[0-9a-f]{40,64}$') { throw 'manifest build identity is invalid.' }
$null = [DateTimeOffset]::ParseExact(
    $manifestObject.publishedAt,
    'yyyy-MM-ddTHH:mm:ssZ',
    [Globalization.CultureInfo]::InvariantCulture,
    [Globalization.DateTimeStyles]::AssumeUniversal
)
if ($manifestObject.package.name -ne 'roost-windows-x64.zip' -or
    $manifestObject.package.sha256 -ne (Get-Sha256 $Package) -or
    [long]$manifestObject.package.size -ne (Get-Item -LiteralPath $Package).Length) {
    throw 'manifest package identity/hash/size mismatch.'
}
if ($manifestObject.shawl.version -ne '1.9.0' -or $manifestObject.shawl.upstreamSha256 -notmatch '^[0-9a-f]{64}$') {
    throw 'manifest Shawl provenance is invalid.'
}

$temp = Join-Path ([IO.Path]::GetTempPath()) ('roost-verify-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $temp | Out-Null
try {
    Expand-Archive -LiteralPath $Package -DestinationPath $temp
    $actualNames = @(Get-ChildItem -LiteralPath $temp -File | ForEach-Object Name | Sort-Object)
    if (($actualNames -join ',') -ne (($payloadNames | Sort-Object) -join ',')) {
        throw "Windows ZIP payload is not exact: $($actualNames -join ', ')"
    }
    if (@(Get-ChildItem -LiteralPath $temp -Directory).Count -ne 0) { throw 'Windows ZIP must have a flat payload.' }

    $manifestFiles = @($manifestObject.files)
    if ($manifestFiles.Count -ne $payloadNames.Count) { throw 'manifest files array does not cover the exact ZIP payload.' }
    $seen = @{}
    foreach ($file in $manifestFiles) {
        $keys = @($file.PSObject.Properties.Name | Sort-Object)
        if (($keys -join ',') -ne 'authenticodeRequired,path,sha256,size') { throw "invalid manifest file record: $($file.path)" }
        if ($payloadNames -notcontains $file.path -or $seen.ContainsKey($file.path)) { throw "unexpected or duplicate manifest path: $($file.path)" }
        $seen[$file.path] = $true
        $path = Join-Path $temp $file.path
        if ($file.sha256 -ne (Get-Sha256 $path) -or [long]$file.size -ne (Get-Item -LiteralPath $path).Length) {
            throw "manifest hash/size mismatch: $($file.path)"
        }
        if ([bool]$file.authenticodeRequired -ne ($authenticodeNames -contains $file.path)) {
            throw "manifest Authenticode policy mismatch: $($file.path)"
        }
    }

    $sumEntries = @{}
    foreach ($line in [IO.File]::ReadAllLines((Join-Path $temp 'SHA256SUMS'), [Text.Encoding]::UTF8)) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        if ($line -notmatch '^([0-9a-f]{64})  ([A-Za-z0-9][A-Za-z0-9._-]*)$') { throw "invalid SHA256SUMS line: $line" }
        if ($sumEntries.ContainsKey($Matches[2])) { throw "duplicate SHA256SUMS path: $($Matches[2])" }
        $sumEntries[$Matches[2]] = $Matches[1]
    }
    foreach ($name in ($payloadNames | Where-Object { $_ -ne 'SHA256SUMS' })) {
        if (-not $sumEntries.ContainsKey($name) -or $sumEntries[$name] -ne (Get-Sha256 (Join-Path $temp $name))) {
            throw "SHA256SUMS mismatch: $name"
        }
    }
    if ($sumEntries.Count -ne ($payloadNames.Count - 1)) { throw 'SHA256SUMS has unexpected entries.' }

    $provenance = Get-Content -LiteralPath (Join-Path $temp 'shawl-v1.9.0.provenance.json') -Raw | ConvertFrom-Json
    if ($provenance.version -ne '1.9.0' -or
        $provenance.binary.sha256 -ne $manifestObject.shawl.upstreamSha256 -or
        $provenance.githubTagSignatureVerified -ne $true) {
        throw 'packaged Shawl provenance does not match the manifest pin.'
    }

    $templates = Get-Content -LiteralPath (Join-Path $temp 'service-templates.json') -Raw | ConvertFrom-Json
    if ($templates.logPolicy.encoding -ne 'utf-8' -or $templates.logPolicy.rotateBytes -ne 2097152 -or $templates.logPolicy.retainedCopies -ne 2) {
        throw 'service log policy is not UTF-8, 2 MiB, two-copy rotation.'
    }
    $expectedServices = [ordered]@{
        keeper = 'RoostKeeperV2'
        worker = 'RoostWorkerV2'
        coordinator = 'RoostCoordinatorV2'
        updater = 'RoostUpdaterV2'
    }
    foreach ($role in $expectedServices.Keys) {
        $service = @($templates.services | Where-Object role -eq $role)
        if ($service.Count -ne 1 -or $service[0].name -ne $expectedServices[$role]) { throw "invalid service template for $role" }
        foreach ($requiredArg in @('--no-restart', '--kill-process-tree', '--stop-timeout', '15000', '--cwd', '--log-dir', '--log-rotate', 'bytes=2097152', '--log-retain', '2')) {
            if ($service[0].shawlArgv -notcontains $requiredArg) { throw "service $role is missing Shawl argument $requiredArg" }
        }
    }
    $worker = @($templates.services | Where-Object role -eq 'worker')[0]
    if ($worker.dependsOn -notcontains 'RoostKeeperV2') { throw 'worker service must depend on keeper.' }
    $coordinator = @($templates.services | Where-Object role -eq 'coordinator')[0]
    if ($coordinator.startMode.coordinatorHost -ne 'automatic' -or $coordinator.startMode.workerOnlyHost -ne 'manual') {
        throw 'coordinator service start policy is invalid.'
    }
    $updater = @($templates.services | Where-Object role -eq 'updater')[0]
    if ($updater.startMode -ne 'automatic') { throw 'updater service must start automatically for transaction recovery.' }

    if (-not $AllowUnsigned) {
        foreach ($name in $authenticodeNames) { Assert-Authenticode (Join-Path $temp $name) $expectedPublisher }
        foreach ($name in $bootstrapScriptNames) {
            Assert-Authenticode (Join-Path $releaseDir $name) $expectedPublisher
        }

        Add-Type -AssemblyName System.Security.Cryptography.Pkcs
        if (-not ('Roost.Windows.ReleaseTimestampVerifier' -as [type])) {
            Add-Type -TypeDefinition @'
using System;
using System.Security.Cryptography;
using System.Security.Cryptography.Pkcs;
using System.Security.Cryptography.X509Certificates;

namespace Roost.Windows {
    public static class ReleaseTimestampVerifier {
        private const string Rfc3161Oid = "1.3.6.1.4.1.311.3.3.1";

        public static void Verify(SignerInfo signer) {
            byte[] publisherSignature = signer.GetSignature();
            foreach (CryptographicAttributeObject attribute in signer.UnsignedAttributes) {
                if (attribute.Oid == null || attribute.Oid.Value != Rfc3161Oid) continue;
                foreach (AsnEncodedData value in attribute.Values) {
                    Rfc3161TimestampToken token;
                    int consumed;
                    byte[] encoded = value.RawData;
                    if (!Rfc3161TimestampToken.TryDecode(
                        new ReadOnlyMemory<byte>(encoded), out token, out consumed) ||
                        consumed != encoded.Length) continue;
                    X509Certificate2 timestampSigner;
                    if (!token.VerifySignatureForData(publisherSignature, out timestampSigner) ||
                        timestampSigner == null) continue;
                    DateTime timestamp = token.TokenInfo.Timestamp.UtcDateTime;
                    if (signer.Certificate == null ||
                        timestamp < signer.Certificate.NotBefore.ToUniversalTime() ||
                        timestamp > signer.Certificate.NotAfter.ToUniversalTime()) continue;
                    using (X509Chain chain = new X509Chain()) {
                        chain.ChainPolicy.ApplicationPolicy.Add(
                            new Oid("1.3.6.1.5.5.7.3.8"));
                        chain.ChainPolicy.VerificationTime = timestamp;
                        chain.ChainPolicy.RevocationMode = X509RevocationMode.Online;
                        chain.ChainPolicy.RevocationFlag = X509RevocationFlag.ExcludeRoot;
                        chain.ChainPolicy.VerificationFlags = X509VerificationFlags.NoFlag;
                        if (!chain.Build(timestampSigner)) continue;
                    }
                    return;
                }
            }
            throw new CryptographicException(
                "manifest CMS does not contain a valid, trusted RFC 3161 timestamp.");
        }
    }
}
'@
        }
        $cms = [Security.Cryptography.Pkcs.SignedCms]::new(
            [Security.Cryptography.Pkcs.ContentInfo]::new([IO.File]::ReadAllBytes($Manifest)),
            $true
        )
        $cms.Decode([IO.File]::ReadAllBytes($Signature))
        $cms.CheckSignature($false)
        if ($cms.SignerInfos.Count -ne 1) { throw 'manifest CMS must have exactly one signer.' }
        $signer = $cms.SignerInfos[0]
        if ($null -eq $signer.Certificate -or (Get-CertificateSha256 $signer.Certificate) -ne $expectedPublisher) {
            throw 'manifest CMS publisher mismatch.'
        }
        $hasCodeSigningEku = $false
        foreach ($extension in $signer.Certificate.Extensions) {
            if ($extension -is [Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension]) {
                foreach ($usage in $extension.EnhancedKeyUsages) {
                    if ($usage.Value -eq '1.3.6.1.5.5.7.3.3') { $hasCodeSigningEku = $true }
                }
            }
        }
        if (-not $hasCodeSigningEku) { throw 'manifest CMS signer lacks Code Signing EKU.' }
        [Roost.Windows.ReleaseTimestampVerifier]::Verify($signer)
    }

    $roostVersion = (& (Join-Path $temp 'roost.exe') version | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or
        ($roostVersion -ne $manifestObject.version -and -not $roostVersion.StartsWith("$($manifestObject.version)+", [StringComparison]::Ordinal))) {
        throw "packaged roost.exe version mismatch: $roostVersion"
    }
    $helperVersion = (& (Join-Path $temp 'roost-win-helper.exe') version | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'packaged helper version failed.' }
    try { $null = $helperVersion | ConvertFrom-Json } catch { throw 'packaged helper version is not protocol/build JSON.' }
    $daclProbe = Join-Path $temp 'account-dacl-probe.txt'
    [IO.File]::WriteAllText($daclProbe, 'probe', [Text.UTF8Encoding]::new($false))
    $currentAccount = [Security.Principal.WindowsIdentity]::GetCurrent().Name
    $daclResult = (& (Join-Path $temp 'roost-win-helper.exe') apply-account-dacl $daclProbe $currentAccount | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'packaged helper account DACL failed.' }
    try {
        $daclJson = $daclResult | ConvertFrom-Json
        if ($daclJson.ok -ne $true) { throw 'account DACL did not return ok.' }
    } catch {
        throw "packaged helper account DACL is not valid protocol JSON: $daclResult"
    }
    $flushDirectory = Join-Path $temp 'flush-directory'
    New-Item -ItemType Directory -Path $flushDirectory | Out-Null
    $flushResult = (& (Join-Path $temp 'roost-win-helper.exe') flush-file $flushDirectory | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'packaged helper could not flush a directory.' }
    try {
        $flushJson = $flushResult | ConvertFrom-Json
        if ($flushJson.ok -ne $true) { throw 'directory flush did not return ok.' }
    } catch {
        throw "packaged helper directory flush is not valid protocol JSON: $flushResult"
    }
} finally {
    if (Test-Path -LiteralPath $temp) { Remove-Item -LiteralPath $temp -Recurse -Force }
}

Write-Output 'Windows release package verified.'

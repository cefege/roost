#requires -Version 5.1
[CmdletBinding()]
param(
    [ValidateSet('coordinator', 'worker')]
    [string] $HostRole = 'coordinator',

    [string] $PublisherSha256 = $env:ROOST_WINDOWS_PUBLISHER_SHA256,

    [string] $ServiceAccount,

    [Security.SecureString] $ServiceAccountPassword,

    [string] $InstallRoot = (Join-Path $env:ProgramData 'Roost'),

    [string] $ReleaseBaseUrl = 'https://github.com/cefege/roost/releases/latest/download'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$script:IcaclsPath = Join-Path ([Environment]::SystemDirectory) 'icacls.exe'
if (-not (Test-Path -LiteralPath $script:IcaclsPath -PathType Leaf)) {
    throw 'trusted Windows icacls.exe is missing'
}

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
    if ($PSVersionTable.PSEdition -eq 'Desktop') {
        Add-Type -AssemblyName System.Security
    } else {
        Add-Type -AssemblyName System.Security.Cryptography.Pkcs
    }
    $contentInfo = [Security.Cryptography.Pkcs.ContentInfo]::new(, $Content)
    $cms = [Security.Cryptography.Pkcs.SignedCms]::new($contentInfo, $true)
    $cms.Decode($Signature)
    $cms.CheckSignature($false)
    if ($cms.SignerInfos.Count -ne 1) {
        throw "release manifest must have exactly one CMS signer; found $($cms.SignerInfos.Count)"
    }
    $certificate = $cms.SignerInfos[0].Certificate
    if ($null -eq $certificate) { throw 'release manifest CMS signer has no certificate' }
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try { $digest = $sha256.ComputeHash($certificate.RawData) }
    finally { $sha256.Dispose() }
    $actual = ([BitConverter]::ToString($digest)).Replace('-', '').ToLowerInvariant()
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
    if ($null -eq $signature.TimeStamperCertificate) {
        throw "missing trusted Authenticode timestamp for $Path"
    }
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try { $digest = $sha256.ComputeHash($signature.SignerCertificate.RawData) }
    finally { $sha256.Dispose() }
    $actual = ([BitConverter]::ToString($digest)).Replace('-', '').ToLowerInvariant()
    if ($actual -cne $ExpectedPublisher) {
        throw "Authenticode publisher mismatch for $Path: expected $ExpectedPublisher, got $actual"
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

function Invoke-VerifiedHelper(
    [string] $Helper,
    [string[]] $Arguments
) {
    Assert-RegularNonReparseFile $Helper 'verified Windows helper'
    $output = (& $Helper @Arguments | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) {
        throw "Windows helper operation '$($Arguments[0])' failed (exit $LASTEXITCODE)"
    }
    return $output
}

function Get-SddlDaclFingerprint([string] $Sddl) {
    $descriptor = [Security.AccessControl.RawSecurityDescriptor]::new($Sddl)
    if ($null -eq $descriptor.DiscretionaryAcl) { throw 'security descriptor has no DACL' }
    $bytes = New-Object byte[] $descriptor.DiscretionaryAcl.BinaryLength
    $descriptor.DiscretionaryAcl.GetBinaryForm($bytes, 0)
    return [Convert]::ToBase64String($bytes)
}

function Get-PathDaclFingerprint([string] $Path) {
    $acl = Get-Acl -LiteralPath $Path
    if (-not $acl.AreAccessRulesProtected) {
        throw "path DACL is not protected from inheritance: $Path"
    }
    $sddl = $acl.GetSecurityDescriptorSddlForm(
        [Security.AccessControl.AccessControlSections]::Access
    )
    return Get-SddlDaclFingerprint $sddl
}

function Get-OwnerSid([string] $Path) {
    $owner = (Get-Acl -LiteralPath $Path).Owner
    try {
        return ([Security.Principal.NTAccount] $owner).Translate(
            [Security.Principal.SecurityIdentifier]
        ).Value
    } catch {
        return ([Security.Principal.SecurityIdentifier] $owner).Value
    }
}

function Assert-ExactSecurityLike([string] $Path, [string] $Reference) {
    if ((Get-OwnerSid $Path) -cne (Get-OwnerSid $Reference) -or
        (Get-PathDaclFingerprint $Path) -cne (Get-PathDaclFingerprint $Reference)) {
        throw "refusing pre-existing path whose owner or exact protected DACL is not canonical: $Path"
    }
}

function New-AdminOnlyDirectory([string] $Path) {
    $security = [Security.AccessControl.DirectorySecurity]::new()
    $security.SetSecurityDescriptorSddlForm(
        'O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)'
    )
    [IO.Directory]::CreateDirectory($Path, $security) | Out-Null
}

function Ensure-ProtectedDirectory(
    [string] $Path,
    [string] $Profile,
    [string] $Helper,
    [string] $Account,
    [string] $InteractiveSid,
    [string] $SecureStagingRoot,
    [switch] $AtomicRoot
) {
    if (Test-Path -LiteralPath $Path) {
        $item = Get-Item -LiteralPath $Path -Force
        if (-not $item.PSIsContainer -or
            ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "refusing unsafe pre-existing Windows install directory: $Path"
        }
        $reference = Join-Path $SecureStagingRoot (
            '.acl-reference-' + [Guid]::NewGuid().ToString('N')
        )
        New-AdminOnlyDirectory $reference
        try {
            $null = Invoke-VerifiedHelper $Helper @(
                'protect-directory', $reference, $Profile, $Account, $InteractiveSid
            )
            Assert-ExactSecurityLike $Path $reference
        } finally {
            Remove-Item -LiteralPath $reference -Recurse -Force -ErrorAction SilentlyContinue
        }
    } elseif ($AtomicRoot) {
        $candidate = Join-Path $SecureStagingRoot (
            '.install-root-' + [Guid]::NewGuid().ToString('N')
        )
        New-AdminOnlyDirectory $candidate
        try {
            $null = Invoke-VerifiedHelper $Helper @(
                'protect-directory', $candidate, $Profile, $Account, $InteractiveSid
            )
            [IO.Directory]::Move($candidate, $Path)
        } finally {
            if (Test-Path -LiteralPath $candidate) {
                Remove-Item -LiteralPath $candidate -Recurse -Force -ErrorAction SilentlyContinue
            }
        }
    } else {
        [IO.Directory]::CreateDirectory($Path) | Out-Null
    }
    $null = Invoke-VerifiedHelper $Helper @(
        'protect-directory', $Path, $Profile, $Account, $InteractiveSid
    )
}


function Write-NoReplaceBytes([string] $Path, [byte[]] $Bytes) {
    $stream = [IO.File]::Open(
        $Path,
        [IO.FileMode]::CreateNew,
        [IO.FileAccess]::Write,
        [IO.FileShare]::None
    )
    try {
        $stream.Write($Bytes, 0, $Bytes.Length)
        $stream.Flush($true)
    } finally {
        $stream.Dispose()
    }
}

function Install-StableLauncher(
    [string] $Root,
    [string] $Helper,
    [string] $SourceHelper,
    [string] $SourceShawl,
    [string] $ExpectedHelperSha256,
    [string] $ExpectedShawlSha256,
    [string] $Publisher
) {
    Assert-RegularNonReparseFile $Helper 'signed bootstrap Windows helper'
    Assert-RegularNonReparseFile $SourceHelper 'signed stable dispatcher source'
    Assert-RegularNonReparseFile $SourceShawl 'signed stable Shawl source'
    if ((Get-Sha256 $SourceHelper) -cne $ExpectedHelperSha256 -or
        (Get-Sha256 $SourceShawl) -cne $ExpectedShawlSha256) {
        throw 'stable-layout source bytes differ from the signed release manifest'
    }
    Assert-Authenticode $SourceHelper $Publisher
    Assert-Authenticode $SourceShawl $Publisher

    $binDir = Join-Path $Root 'bin'
    $allowed = @(
        'shawl.exe',
        'roost.exe',
        'install-root.txt',
        'publisher.sha256'
    )
    foreach ($entry in @(Get-ChildItem -LiteralPath $binDir -Force)) {
        if ($allowed -notcontains $entry.Name) {
            throw "refusing unexpected pre-existing stable-layout entry: $($entry.FullName)"
        }
        Assert-RegularNonReparseFile $entry.FullName 'pre-existing stable-layout file'
    }

    foreach ($entry in @(
        @($SourceHelper, (Join-Path $binDir 'roost.exe'), 'stable-launcher', $ExpectedHelperSha256),
        @($SourceShawl, (Join-Path $binDir 'shawl.exe'), 'stable-shawl', $ExpectedShawlSha256)
    )) {
        $source = [string] $entry[0]
        $destination = [string] $entry[1]
        $profile = [string] $entry[2]
        $expectedSha256 = [string] $entry[3]
        if (Test-Path -LiteralPath $destination) {
            Assert-RegularNonReparseFile $destination 'pre-existing stable-layout executable'
            $null = Invoke-VerifiedHelper $Helper @(
                'protect-updater-artifact', $destination, $profile
            )
        }
        $null = Invoke-VerifiedHelper $Helper @(
            'copy-updater-artifact', $source, $destination,
            'release', $profile, $expectedSha256,
            [string] ([IO.FileInfo] $source).Length
        )
        $null = Invoke-VerifiedHelper $Helper @(
            'inspect-updater-artifact', $destination, $profile,
            $expectedSha256, [string] ([IO.FileInfo] $destination).Length
        )
    }

    $staged = [ordered]@{
        'install-root.txt' = Join-Path $binDir ('.install-root-' + [Guid]::NewGuid().ToString('N') + '.tmp')
        'publisher.sha256' = Join-Path $binDir ('.publisher-' + [Guid]::NewGuid().ToString('N') + '.tmp')
    }
    try {
        Write-NoReplaceBytes $staged['install-root.txt'] (
            [Text.UTF8Encoding]::new($false).GetBytes($Root + "`r`n")
        )
        Write-NoReplaceBytes $staged['publisher.sha256'] (
            [Text.ASCIIEncoding]::new().GetBytes($Publisher + "`r`n")
        )
        foreach ($name in $staged.Keys) {
            $temporary = $staged[$name]
            $destination = Join-Path $binDir $name
            $null = Invoke-VerifiedHelper $Helper @(
                'protect-updater-artifact', $temporary, 'release'
            )
            $null = Invoke-VerifiedHelper $Helper @('flush-file', $temporary)
            $null = Invoke-VerifiedHelper $Helper @(
                'replace-file', $temporary, $destination
            )
            $null = Invoke-VerifiedHelper $Helper @('flush-file', $destination)
            $null = Invoke-VerifiedHelper $Helper @(
                'inspect-updater-artifact', $destination, 'release'
            )
        }
        $null = Invoke-VerifiedHelper $Helper @('flush-file', $binDir)
    } finally {
        foreach ($temporary in $staged.Values) {
            if (Test-Path -LiteralPath $temporary) {
                Remove-Item -LiteralPath $temporary -Force -ErrorAction SilentlyContinue
            }
        }
    }
    if ((Get-Sha256 (Join-Path $binDir 'roost.exe')) -cne $ExpectedHelperSha256 -or
        (Get-Sha256 (Join-Path $binDir 'shawl.exe')) -cne $ExpectedShawlSha256) {
        throw 'installed stable-layout executable checksum mismatch'
    }

    $legacyBinDir = if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        $null
    } else {
        Join-Path $env:LOCALAPPDATA 'RoostCLI'
    }
    $keepPathSegment = {
        param([string] $Segment)
        if ([string]::IsNullOrWhiteSpace($Segment)) { return $false }
        if ($Segment.TrimEnd('\') -ieq $binDir.TrimEnd('\')) { return $false }
        return $null -eq $legacyBinDir -or $Segment.TrimEnd('\') -ine $legacyBinDir.TrimEnd('\')
    }
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $segments = @($userPath -split ';' | Where-Object $keepPathSegment)
    [Environment]::SetEnvironmentVariable(
        'Path',
        (@($binDir) + $segments -join ';'),
        'User'
    )
}

function New-AdminStagingDirectory() {
    if ([string]::IsNullOrWhiteSpace($env:ProgramData)) { throw 'ProgramData is required' }
    $parent = [IO.Path]::GetFullPath($env:ProgramData)
    $parentItem = Get-Item -LiteralPath $parent -Force
    if (($parentItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw 'refusing reparse-point ProgramData directory'
    }
    $parentOwner = ([Security.Principal.NTAccount](Get-Acl -LiteralPath $parent).Owner).Translate([Security.Principal.SecurityIdentifier]).Value
    if ($parentOwner -notin @('S-1-5-18', 'S-1-5-32-544')) {
        throw 'ProgramData must be owned by SYSTEM or Administrators'
    }
    $staging = Join-Path $parent ('RoostBootstrap-' + [Guid]::NewGuid().ToString('N'))
    $security = [Security.AccessControl.DirectorySecurity]::new()
    $security.SetSecurityDescriptorSddlForm(
        'O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)'
    )
    try {
        [IO.Directory]::CreateDirectory($staging, $security) | Out-Null
        if (((Get-Item -LiteralPath $staging -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw 'refusing reparse-point Windows bootstrap staging directory'
        }
        & $script:IcaclsPath $staging '/setintegritylevel' '(OI)(CI)H' | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'failed to apply high-integrity staging policy' }
        return $staging
    } catch {
        Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue
        throw
    }
}
function Assert-CanonicalInstallRoot([string] $Path) {
    if ([string]::IsNullOrWhiteSpace($env:ProgramData)) { throw 'ProgramData is required' }
    $programData = [IO.Path]::GetFullPath($env:ProgramData).TrimEnd('\')
    $knownProgramData = [IO.Path]::GetFullPath(
        [Environment]::GetFolderPath([Environment+SpecialFolder]::CommonApplicationData)
    ).TrimEnd('\')
    if (-not $programData.Equals($knownProgramData, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'ProgramData disagrees with the trusted CommonApplicationData location'
    }
    $expected = [IO.Path]::GetFullPath((Join-Path $programData 'Roost')).TrimEnd('\')
    if (-not ([IO.Path]::GetFullPath($Path).TrimEnd('\')).Equals(
        $expected,
        [StringComparison]::OrdinalIgnoreCase
    )) {
        throw 'custom Windows install roots are not safely supportable; use %ProgramData%\Roost'
    }
    $cursor = $programData
    while (-not [string]::IsNullOrWhiteSpace($cursor)) {
        $item = Get-Item -LiteralPath $cursor -Force
        if (-not $item.PSIsContainer -or
            ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "trusted ProgramData path contains an unsafe component: $cursor"
        }
        $parent = [IO.Directory]::GetParent($cursor)
        if ($null -eq $parent) { break }
        $cursor = $parent.FullName
    }
    $owner = Get-OwnerSid $programData
    if ($owner -notin @('S-1-5-18', 'S-1-5-32-544')) {
        throw 'ProgramData must be owned by SYSTEM or Administrators'
    }
}


function Get-LocalServiceAccountName([string] $Account) {
    $parts = $Account -split '\\', 2
    if ($parts.Count -eq 1) {
        $name = $parts[0]
    } elseif ($parts[0] -eq '.' -or $parts[0] -ieq $env:COMPUTERNAME) {
        $name = $parts[1]
    } else {
        return $null
    }
    if ($name -notmatch '^[A-Za-z0-9][A-Za-z0-9._-]{0,19}$') {
        throw 'the dedicated local Roost service account name is invalid'
    }
    return $name
}

function New-RandomServiceAccountPassword {
    $bytes = New-Object byte[] 48
    $rng = [Security.Cryptography.RandomNumberGenerator]::Create()
    try {
        $rng.GetBytes($bytes)
        $password = [Security.SecureString]::new()
        foreach ($required in @('R', 'r', '7', '!')) {
            $password.AppendChar([char] $required)
        }
        $alphabet = 'ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789!#$%&*+-=?@'
        foreach ($value in $bytes) {
            $password.AppendChar($alphabet[[int] $value % $alphabet.Length])
        }
        $password.MakeReadOnly()
        return $password
    } finally {
        [Array]::Clear($bytes, 0, $bytes.Length)
        $rng.Dispose()
    }
}


function Ensure-DedicatedLocalServiceAccount([string] $Account, [Security.SecureString] $Password) {
    $name = Get-LocalServiceAccountName $Account
    if ($null -eq $name) { return $null }
    $existing = Get-LocalUser -Name $name -ErrorAction SilentlyContinue
    if ($null -eq $existing) {
        New-LocalUser -Name $name -Password $Password -AccountNeverExpires `
            -PasswordNeverExpires -Description 'Restricted Roost service identity' | Out-Null
        $created = $name
    } else {
        $created = $null
    }
    $script:ServiceAccount = "$env:COMPUTERNAME\$name"
    return $created
}

if (-not [IO.Path]::IsPathRooted($InstallRoot)) {
    throw 'InstallRoot must be an absolute path'
}
$InstallRoot = [IO.Path]::GetFullPath($InstallRoot)
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) { throw 'install-binary.ps1 is Windows-only' }
if (-not [Environment]::Is64BitOperatingSystem) {
    throw 'the signed Roost Windows release currently supports x64 Windows only'
}
if (-not [Environment]::Is64BitProcess) {
    throw 'install-binary.ps1 requires 64-bit Windows PowerShell; run it from %SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe'
}
Assert-CanonicalInstallRoot $InstallRoot
if ([string]::IsNullOrWhiteSpace($ServiceAccount)) {
    $ServiceAccount = "$env:COMPUTERNAME\roost-operator"
}
if (-not $ServiceAccountPassword) {
    $localName = Get-LocalServiceAccountName $ServiceAccount
    $existingLocalAccount = if ($null -eq $localName) {
        $null
    } else {
        Get-LocalUser -Name $localName -ErrorAction SilentlyContinue
    }
    if ($null -ne $localName -and $null -eq $existingLocalAccount) {
        $ServiceAccountPassword = New-RandomServiceAccountPassword
    } else {
        $credential = Get-Credential -UserName $ServiceAccount `
            -Message 'Credential for the existing dedicated non-administrator Roost service account'
        $ServiceAccount = $credential.UserName
        $ServiceAccountPassword = $credential.Password
    }
}
if ([string]::IsNullOrWhiteSpace($ServiceAccount)) {
    throw 'a dedicated non-administrator Roost service account is required'
}
if ($ServiceAccount -match '^(?i:LocalSystem|SYSTEM|NT AUTHORITY\\SYSTEM|\.\\LocalSystem)$') {
    throw 'LocalSystem is forbidden; choose a dedicated non-administrator Roost service account'
}

$publisher = Normalize-Sha256 $PublisherSha256
Assert-HexSha256 $publisher 'ROOST_WINDOWS_PUBLISHER_SHA256'
Assert-Authenticode $PSCommandPath $publisher

$manifestName = 'roost-windows-x64.manifest.json'
$signatureName = 'roost-windows-x64.manifest.json.p7s'
$packageName = 'roost-windows-x64.zip'
$manifestUrl = "$($ReleaseBaseUrl.TrimEnd('/'))/$manifestName"
$signatureUrl = "$($ReleaseBaseUrl.TrimEnd('/'))/$signatureName"
$packageUrl = "$($ReleaseBaseUrl.TrimEnd('/'))/$packageName"
$interactiveSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
if ($interactiveSid -notmatch '^S-1-[0-9-]+$') { throw 'interactive account SID is invalid' }
$oldInteractiveSid = $env:ROOST_INTERACTIVE_SID
$env:ROOST_INTERACTIVE_SID = $interactiveSid
$tempRoot = New-AdminStagingDirectory
$createdServiceAccount = $null
$servicesCommitted = $false
$bootstrapHelper = $null
$currentPath = $null
$currentChanged = $false
$currentWasPresent = $false
[byte[]] $priorCurrentBytes = $null
$installFailure = $null

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
    $manifest = [Text.Encoding]::UTF8.GetString($manifestBytes) | ConvertFrom-Json

    if ($manifest.schemaVersion -ne 1 -or $manifest.platform -cne 'win32' -or $manifest.arch -cne 'x64') {
        throw 'unsupported Windows release manifest identity'
    }
    if ($manifest.package.name -cne $packageName) { throw 'manifest package name is not canonical' }
    Assert-HexSha256 ([string] $manifest.package.sha256) 'manifest package SHA-256'
    if ([long] $manifest.package.size -le 0) { throw 'manifest package size is invalid' }
    if ($manifest.shawl.version -cne '1.9.0') { throw 'manifest Shawl version must be 1.9.0' }
    Assert-HexSha256 ([string] $manifest.shawl.upstreamSha256) 'manifest Shawl upstream SHA-256'
    if ([string]::IsNullOrWhiteSpace([string] $manifest.version)) { throw 'manifest version is missing' }
    if ([string] $manifest.build -notmatch '^[0-9a-f]{40,64}$') { throw 'manifest build identity is invalid' }
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
    # Mutate local identity state only after the complete signed payload has
    # passed CMS, manifest, ZIP, checksum, and Authenticode verification.
    $createdServiceAccount = Ensure-DedicatedLocalServiceAccount $ServiceAccount $ServiceAccountPassword

    $bootstrapHelper = Join-Path $packageRoot 'roost-win-helper.exe'
    Assert-RegularNonReparseFile $bootstrapHelper 'signed bootstrap Windows helper'
    $accountProbe = Invoke-VerifiedHelper $bootstrapHelper @(
        'resolve-account-sid', $ServiceAccount
    )
    try { $accountIdentity = $accountProbe | ConvertFrom-Json }
    catch { throw "Windows helper returned invalid service-account metadata: $accountProbe" }
    if ([string]::IsNullOrWhiteSpace([string] $accountIdentity.canonicalAccount) -or
        $null -eq $accountIdentity.administrator -or
        $null -eq $accountIdentity.localAccount) {
        throw 'Windows helper omitted service-account privilege metadata'
    }
    if ([bool] $accountIdentity.administrator) {
        throw 'Roost Windows services require a dedicated non-administrator service account'
    }
    $ServiceAccount = [string] $accountIdentity.canonicalAccount

    $serviceDir = Join-Path $InstallRoot 'service'
    $versionsDir = Join-Path $InstallRoot 'versions'
    $binDir = Join-Path $InstallRoot 'bin'
    Ensure-ProtectedDirectory $InstallRoot 'install-root' $bootstrapHelper `
        $ServiceAccount $interactiveSid $tempRoot -AtomicRoot
    Ensure-ProtectedDirectory $binDir 'stable-bin' $bootstrapHelper `
        $ServiceAccount $interactiveSid $tempRoot
    Ensure-ProtectedDirectory $versionsDir 'versions-root' $bootstrapHelper `
        $ServiceAccount $interactiveSid $tempRoot
    Ensure-ProtectedDirectory $serviceDir 'service-root' $bootstrapHelper `
        $ServiceAccount $interactiveSid $tempRoot

    $helperFiles = @($manifest.files | Where-Object {
        ([string] $_.path).Replace('\', '/') -ceq 'roost-win-helper.exe'
    })
    $shawlFiles = @($manifest.files | Where-Object {
        ([string] $_.path).Replace('\', '/') -ceq 'shawl.exe'
    })
    if ($helperFiles.Count -ne 1 -or $shawlFiles.Count -ne 1) {
        throw 'signed release manifest must contain exactly one helper and one Shawl executable'
    }
    $helperSha256 = Normalize-Sha256 ([string] $helperFiles[0].sha256)
    $shawlSha256 = Normalize-Sha256 ([string] $shawlFiles[0].sha256)

    # Stage first so the protected current manifest exists before updater/SCM
    # services can be installed or started.
    & $packageInstaller -HostRole $HostRole -InstallRoot $InstallRoot `
        -ServiceDir $serviceDir -PublisherSha256 $publisher -ServiceAccount $ServiceAccount `
        -ServiceAccountPassword $ServiceAccountPassword -ExpectedVersion ([string] $manifest.version) `
        -ExpectedBuild ([string] $manifest.build) -StageOnly | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "signed package staging failed (exit $LASTEXITCODE)" }

    $versionDir = Join-Path $versionsDir ([string] $manifest.version)
    if (-not (Test-Path -LiteralPath $versionDir -PathType Container)) {
        throw "signed package did not stage expected version directory: $versionDir"
    }
    $current = [ordered]@{
        schemaVersion = 2
        version = [string] $manifest.version
        build = [string] $manifest.build
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
    if (Test-Path -LiteralPath $currentPath) {
        Assert-RegularNonReparseFile $currentPath 'pre-existing current manifest'
        $priorCurrentBytes = [IO.File]::ReadAllBytes($currentPath)
        $currentWasPresent = $true
    }
    $currentTemp = Join-Path $serviceDir ('.current-' + [Guid]::NewGuid().ToString('N') + '.tmp')
    Write-NoReplaceBytes $currentTemp (
        [Text.UTF8Encoding]::new($false).GetBytes(
            (($current | ConvertTo-Json -Depth 16) + "`n")
        )
    )
    try {
        $null = Invoke-VerifiedHelper $bootstrapHelper @('flush-file', $currentTemp)
        $null = Invoke-VerifiedHelper $bootstrapHelper @(
            'replace-file', $currentTemp, $currentPath
        )
    } finally {
        if (Test-Path -LiteralPath $currentTemp) {
            Remove-Item -LiteralPath $currentTemp -Force -ErrorAction SilentlyContinue
        }
    }
    $currentChanged = $true
    $null = Invoke-VerifiedHelper $bootstrapHelper @(
        'protect-updater-artifact', $currentPath, 'current'
    )
    $null = Invoke-VerifiedHelper $bootstrapHelper @('flush-file', $currentPath)

    Install-StableLauncher $InstallRoot $bootstrapHelper `
        (Join-Path $versionDir 'roost-win-helper.exe') `
        (Join-Path $versionDir 'shawl.exe') $helperSha256 $shawlSha256 `
        $publisher

    $oldServiceDir = $env:ROOST_SERVICE_DIR
    $oldPublisher = $env:ROOST_WINDOWS_PUBLISHER_SHA256
    try {
        $env:ROOST_SERVICE_DIR = $serviceDir
        $env:ROOST_WINDOWS_PUBLISHER_SHA256 = $publisher
        & $packageInstaller -HostRole $HostRole -InstallRoot $InstallRoot `
            -ServiceDir $serviceDir -PublisherSha256 $publisher -ServiceAccount $ServiceAccount `
            -ServiceAccountPassword $ServiceAccountPassword -ExpectedVersion ([string] $manifest.version) `
            -ExpectedBuild ([string] $manifest.build)
        if ($LASTEXITCODE -ne 0) { throw "signed package install failed (exit $LASTEXITCODE)" }
        $servicesCommitted = $true
    } finally {
        $env:ROOST_SERVICE_DIR = $oldServiceDir
        $env:ROOST_WINDOWS_PUBLISHER_SHA256 = $oldPublisher
    }
} catch {
    $installFailure = $_
    throw
} finally {
    $rollbackFailure = $null
    try {
        if (-not $servicesCommitted -and $currentChanged -and $bootstrapHelper -and $currentPath) {
            if ($currentWasPresent) {
                $rollbackTemp = Join-Path (Split-Path -Parent $currentPath) (
                    '.current-rollback-' + [Guid]::NewGuid().ToString('N') + '.tmp'
                )
                Write-NoReplaceBytes $rollbackTemp $priorCurrentBytes
                try {
                    $null = Invoke-VerifiedHelper $bootstrapHelper @('flush-file', $rollbackTemp)
                    if (Test-Path -LiteralPath $currentPath) {
                        Assert-RegularNonReparseFile $currentPath 'failed-install current manifest'
                    }
                    $null = Invoke-VerifiedHelper $bootstrapHelper @(
                        'replace-file', $rollbackTemp, $currentPath
                    )
                } finally {
                    if (Test-Path -LiteralPath $rollbackTemp) {
                        Remove-Item -LiteralPath $rollbackTemp -Force -ErrorAction SilentlyContinue
                    }
                }
                $null = Invoke-VerifiedHelper $bootstrapHelper @(
                    'protect-updater-artifact', $currentPath, 'current'
                )
                $null = Invoke-VerifiedHelper $bootstrapHelper @('flush-file', $currentPath)
            } else {
                $null = Invoke-VerifiedHelper $bootstrapHelper @('remove-file', $currentPath)
            }
        }
    } catch {
        $rollbackFailure = $_
    }

    if (-not $servicesCommitted -and $createdServiceAccount -and $bootstrapHelper) {
        $accountIsStillRequired = $false
        foreach ($serviceName in @(
            'RoostKeeperV2',
            'RoostWorkerV2',
            'RoostCoordinatorV2',
            'RoostUpdaterV2'
        )) {
            try {
                $rawService = Invoke-VerifiedHelper $bootstrapHelper @(
                    'service-query', $serviceName, 'basic'
                )
                $serviceState = $rawService | ConvertFrom-Json
                if ([bool] $serviceState.installed) { $accountIsStillRequired = $true }
            } catch {
                $accountIsStillRequired = $true
            }
        }
        if (-not $accountIsStillRequired) {
            Remove-LocalUser -Name $createdServiceAccount -ErrorAction SilentlyContinue
        }
    }
    $env:ROOST_INTERACTIVE_SID = $oldInteractiveSid
    if (Test-Path -LiteralPath $tempRoot) {
        Remove-Item -LiteralPath $tempRoot -Recurse -Force
    }
    if ($rollbackFailure) {
        $original = if ($installFailure) { $installFailure.Exception.Message } else { 'unknown install failure' }
        throw "Windows install failed ($original) and current-manifest rollback failed: $($rollbackFailure.Exception.Message)"
    }
}

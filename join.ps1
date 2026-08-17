#requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^https://')]
    [string] $CoordinatorUrl,
    [string] $WorkerLabel,


    [Security.SecureString] $BootstrapToken,

    [string] $PublisherSha256 = $env:ROOST_WINDOWS_PUBLISHER_SHA256,

    [string] $ServiceAccount,

    [Security.SecureString] $ServiceAccountPassword,

    [string] $InstallRoot = (Join-Path $env:ProgramData 'Roost'),

    [string] $ReleaseBaseUrl = 'https://github.com/cefege/roost/releases/latest/download',

    [string] $InstallerPath = (Join-Path $PSScriptRoot 'install-binary.ps1')
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

function Get-SignedPublisher([string] $Path) {
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.Status -ne [Management.Automation.SignatureStatus]::Valid -or
        $null -eq $signature.SignerCertificate -or
        $null -eq $signature.TimeStamperCertificate) {
        throw "$([IO.Path]::GetFileName($Path)) has no valid Authenticode signature and trusted timestamp ($($signature.Status))"
    }
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try { $digest = $sha256.ComputeHash($signature.SignerCertificate.RawData) }
    finally { $sha256.Dispose() }
    return ([BitConverter]::ToString($digest)).Replace('-', '').ToLowerInvariant()
}

function Assert-SignedInstaller([string] $Path, [string] $ExpectedPublisher) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "install-binary.ps1 not found at $Path"
    }
    $actual = Get-SignedPublisher $Path
    if ($actual -cne $ExpectedPublisher) {
        throw "install-binary.ps1 publisher mismatch: expected $ExpectedPublisher, got $actual"
    }
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

if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) { throw 'join.ps1 is Windows-only' }
if (-not [Environment]::Is64BitOperatingSystem) {
    throw 'the signed Roost Windows release currently supports x64 Windows only'
}
if (-not [Environment]::Is64BitProcess) {
    throw 'join.ps1 requires 64-bit Windows PowerShell; run it from %SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe'
}
$publisher = Normalize-Sha256 $PublisherSha256
if ($publisher -notmatch '^[0-9a-f]{64}$') {
    throw 'ROOST_WINDOWS_PUBLISHER_SHA256 must independently pin the 64-hex signing leaf certificate SHA-256'
}
$scriptPublisher = Get-SignedPublisher $PSCommandPath
if ($scriptPublisher -cne $publisher) {
    throw "join.ps1 publisher mismatch: expected $publisher, got $scriptPublisher"
}
$tailscalePath = Join-Path ([Environment]::GetFolderPath(
    [Environment+SpecialFolder]::ProgramFiles
)) 'Tailscale\tailscale.exe'
if (-not (Test-Path -LiteralPath $tailscalePath -PathType Leaf) -or
    ((Get-Item -LiteralPath $tailscalePath -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
    throw 'trusted machine-wide Tailscale is required. Install it from https://tailscale.com/download/windows and connect before joining Roost.'
}
$tailscaleSignature = Get-AuthenticodeSignature -LiteralPath $tailscalePath
if ($tailscaleSignature.Status -ne [Management.Automation.SignatureStatus]::Valid) {
    throw 'the machine-wide Tailscale executable does not have a valid Authenticode signature'
}
$tailscale = (& $tailscalePath status --json | Out-String) | ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or $tailscale.BackendState -cne 'Running') {
    throw 'Tailscale must be connected before joining Roost'
}
if (-not $BootstrapToken) {
    $BootstrapToken = Read-Host -AsSecureString 'One-shot Roost worker bootstrap token'
}
if ([string]::IsNullOrWhiteSpace($ServiceAccount)) {
    $ServiceAccount = "$env:COMPUTERNAME\roost-operator"
}
if (-not $ServiceAccountPassword) {
    $credential = Get-Credential -UserName $ServiceAccount `
        -Message 'Credential for the dedicated non-administrator Roost service account (it is created if absent)'
    $ServiceAccount = $credential.UserName
    $ServiceAccountPassword = $credential.Password
}
if ([string]::IsNullOrWhiteSpace($ServiceAccount)) {
    throw 'a dedicated non-administrator Roost service account is required'
}
$installerStagingRoot = $null
$tokenBstr = [IntPtr]::Zero
$oldCoordinatorUrl = $env:ROOST_COORDINATOR_URL
$oldBootstrapToken = $env:ROOST_BOOTSTRAP_TOKEN
$oldPublisher = $env:ROOST_WINDOWS_PUBLISHER_SHA256
$oldWorkerLabel = $env:ROOST_WORKER_LABEL
try {
    $requestedInstaller = $InstallerPath
    $installerStagingRoot = New-AdminStagingDirectory
    $InstallerPath = Join-Path $installerStagingRoot 'install-binary.ps1'
    if ($PSBoundParameters.ContainsKey('InstallerPath')) {
        Copy-Item -LiteralPath $requestedInstaller -Destination $InstallerPath
    } else {
        Invoke-WebRequest -UseBasicParsing "$($ReleaseBaseUrl.TrimEnd('/'))/install-binary.ps1" -OutFile $InstallerPath
    }
    Assert-SignedInstaller $InstallerPath $publisher
    $tokenBstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($BootstrapToken)
    $token = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($tokenBstr)
    if ([string]::IsNullOrWhiteSpace($token)) { throw 'bootstrap token must not be empty' }

    # These values exist only in this installer process tree. The strict worker
    # enrollment consumes the token before any SCM definition is written; the
    # token is never placed in a service ImagePath or durable file.
    $env:ROOST_COORDINATOR_URL = $CoordinatorUrl
    $env:ROOST_BOOTSTRAP_TOKEN = $token
    $env:ROOST_WINDOWS_PUBLISHER_SHA256 = $publisher
    if ([string]::IsNullOrWhiteSpace($WorkerLabel)) {
        $null = Remove-Item Env:ROOST_WORKER_LABEL -ErrorAction SilentlyContinue
    } else {
        $env:ROOST_WORKER_LABEL = $WorkerLabel
    }

    & $InstallerPath -HostRole worker -PublisherSha256 $publisher `
        -ServiceAccount $ServiceAccount -ServiceAccountPassword $ServiceAccountPassword `
        -InstallRoot $InstallRoot -ReleaseBaseUrl $ReleaseBaseUrl
} finally {
    $env:ROOST_COORDINATOR_URL = $oldCoordinatorUrl
    $env:ROOST_BOOTSTRAP_TOKEN = $oldBootstrapToken
    $env:ROOST_WINDOWS_PUBLISHER_SHA256 = $oldPublisher
    $env:ROOST_WORKER_LABEL = $oldWorkerLabel
    if ($tokenBstr -ne [IntPtr]::Zero) {
        [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($tokenBstr)
    }
    if ($installerStagingRoot) { Remove-Item -LiteralPath $installerStagingRoot -Recurse -Force -ErrorAction SilentlyContinue }
    $token = $null
}

#requires -Version 5.1
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ($PSVersionTable.PSEdition -ne 'Desktop' -or $PSVersionTable.PSVersion.Major -ne 5) {
    throw "expected Windows PowerShell 5.1, got $($PSVersionTable.PSEdition) $($PSVersionTable.PSVersion)"
}

$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$scripts = @(
    (Join-Path $root 'join.ps1'),
    (Join-Path $root 'install-binary.ps1'),
    (Join-Path $root 'assets\windows\install.ps1'),
    (Join-Path $root 'assets\windows\provision-service-account.ps1')
)
foreach ($path in $scripts) {
    $tokens = $null
    $errors = $null
    $ast = [Management.Automation.Language.Parser]::ParseFile($path, [ref] $tokens, [ref] $errors)
    if ($errors.Count -ne 0) {
        throw "$path does not parse in Windows PowerShell 5.1: $($errors[0].Message)"
    }
    $bareCommands = @($ast.FindAll({
        param($node)
        if ($node -isnot [Management.Automation.Language.CommandAst]) { return $false }
        return $node.GetCommandName() -in @('icacls.exe', 'sc.exe', 'tailscale.exe')
    }, $true))
    if ($bareCommands.Count -ne 0) {
        throw "$path invokes a security-critical executable by bare name: $($bareCommands[0].GetCommandName())"
    }
    $unsupportedRuntimeTypes = @($ast.FindAll({
        param($node)
        return $node -is [Management.Automation.Language.TypeExpressionAst] -and
            $node.TypeName.FullName -eq 'Runtime.InteropServices.RuntimeInformation'
    }, $true))
    if ($unsupportedRuntimeTypes.Count -ne 0) {
        throw "$path uses RuntimeInformation, which is unavailable on the complete Windows PowerShell 5.1 runtime floor"
    }
}

Add-Type -AssemblyName System.Security
$content = [Security.Cryptography.Pkcs.ContentInfo]::new(, [byte[]] @(1, 2, 3))
$cms = [Security.Cryptography.Pkcs.SignedCms]::new($content, $true)
if ($null -eq $cms) { throw 'SignedCms is unavailable in Windows PowerShell 5.1' }

Write-Output 'Windows PowerShell 5.1 bootstrap compatibility verified.'

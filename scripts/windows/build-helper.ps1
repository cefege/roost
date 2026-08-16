#requires -Version 5.1
[CmdletBinding()]
param(
    [string] $SourceDir = 'native/windows',
    [string] $BuildDir = 'dist/native-windows',
    [string] $OutFile = 'dist/roost-win-helper.exe'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw 'roost-win-helper must be built on Windows with the Windows SDK.'
}
foreach ($tool in @('cmake.exe', 'cl.exe', 'link.exe', 'dumpbin.exe')) {
    if ($null -eq (Get-Command $tool -ErrorAction SilentlyContinue)) {
        throw "$tool is required; initialize a Visual Studio 2022 x64 developer environment first."
    }
}
if (-not (Test-Path -LiteralPath (Join-Path $SourceDir 'CMakeLists.txt') -PathType Leaf)) {
    throw "missing native helper project: $SourceDir/CMakeLists.txt"
}

$source = [IO.Path]::GetFullPath($SourceDir)
$build = [IO.Path]::GetFullPath($BuildDir)
$output = [IO.Path]::GetFullPath($OutFile)
New-Item -ItemType Directory -Path $build -Force | Out-Null
New-Item -ItemType Directory -Path ([IO.Path]::GetDirectoryName($output)) -Force | Out-Null

$configureArgs = @(
    '-S', $source,
    '-B', $build,
    '-G', 'Visual Studio 17 2022',
    '-A', 'x64',
    '-T', 'host=x64',
    '-DCMAKE_POLICY_DEFAULT_CMP0091=NEW',
    '-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded',
    '-DCMAKE_C_FLAGS_RELEASE=/O2 /Ob2 /DNDEBUG /MT',
    '-DCMAKE_CXX_FLAGS_RELEASE=/O2 /Ob2 /DNDEBUG /MT'
)
& cmake.exe @configureArgs
if ($LASTEXITCODE -ne 0) { throw "CMake configure failed with exit code $LASTEXITCODE" }

$buildArgs = @('--build', $build, '--config', 'Release', '--verbose')
& cmake.exe @buildArgs
if ($LASTEXITCODE -ne 0) { throw "CMake build failed with exit code $LASTEXITCODE" }

$candidates = @(
    (Join-Path $build 'Release/roost-win-helper.exe'),
    (Join-Path $build 'roost-win-helper.exe')
) | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf }
if ($candidates.Count -ne 1) {
    throw "expected exactly one roost-win-helper.exe build output; found $($candidates.Count)"
}
[IO.File]::Copy([IO.Path]::GetFullPath($candidates[0]), $output, $true)

$dependencies = (& dumpbin.exe /nologo /dependents $output | Out-String)
if ($LASTEXITCODE -ne 0) { throw "dumpbin failed with exit code $LASTEXITCODE" }
if ($dependencies -match '(?im)^\s*(?:VCRUNTIME|MSVCP|ucrtbase|api-ms-win-crt)[^\s]*\.dll\s*$') {
    throw "roost-win-helper.exe imports a dynamic MSVC/UCRT runtime; the release helper must be built with /MT.`n$dependencies"
}

Write-Output $output

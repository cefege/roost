# Roost installer for Windows 11 x64. A Windows machine only ever joins an
# existing coordinator (`roost join`); copy the line `roost add-machine
# --platform windows` prints on the coordinator host into a non-elevated
# PowerShell:
#
#   $env:ROOST_COORDINATOR_URL='https://roost.example.com'; $env:ROOST_BOOTSTRAP_TOKEN='roost_bt_…'; irm https://raw.githubusercontent.com/cefege/roost/v3/install.ps1 | iex
#
# What it does: pick the newest v3 release the same way install.sh does, fetch
# its `roost-windows-x64.exe` and `roost-keeper-windows-x64.exe`, check both
# against the digests published beside them, run `roost join` (which installs
# the release under %LOCALAPPDATA%\RoostWorkerV3 and registers the
# \Roost\roost3-worker logon task), then put the `roost` shim on the user Path.
#
# THE BODY IS ONE FUNCTION, CALLED ON THE LAST LINE. `irm | iex` evaluates what
# it downloaded, and a connection cut mid-file would otherwise run half of it.
#
# WHAT THE DIGEST PROVES: the sidecar and the asset come from the same origin,
# so a successful check says the program matches the digest published beside
# it, and nothing more.
#
# The origin is overridable by the same variables install.sh and `roost update`
# read: ROOST_RELEASE_API_URL, ROOST_RELEASE_BASE_URL, ROOST_RELEASE_CHANNEL.

function Install-RoostWorker {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'

    if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
        throw "Roost publishes a Windows worker for x64 only; this machine is $($env:PROCESSOR_ARCHITECTURE)."
    }
    if (-not $env:ROOST_COORDINATOR_URL -or -not $env:ROOST_BOOTSTRAP_TOKEN) {
        throw 'Windows machines join an existing coordinator; get the command from roost add-machine --platform windows'
    }
    $channel = $env:ROOST_RELEASE_CHANNEL
    if ($channel -and $channel -ne 'stable' -and $channel -ne 'prerelease') {
        throw "ROOST_RELEASE_CHANNEL must be stable or prerelease, not '$channel'."
    }

    $api = if ($env:ROOST_RELEASE_API_URL) { $env:ROOST_RELEASE_API_URL } else { 'https://api.github.com/repos/cefege/roost/releases?per_page=100' }
    $downloads = if ($env:ROOST_RELEASE_BASE_URL) { $env:ROOST_RELEASE_BASE_URL } else { 'https://github.com/cefege/roost/releases/download' }

    $tag = Select-RoostTag -Api $api -Channel $channel
    if (-not $tag) {
        throw 'No v3 release is published for this channel; nothing was installed.'
    }

    $stage = Join-Path $env:TEMP ('roost-install-' + [guid]::NewGuid())
    New-Item -ItemType Directory -Path $stage | Out-Null
    try {
        $assets = @(
            @('roost-windows-x64.exe', 'roost.exe'),
            @('roost-keeper-windows-x64.exe', 'roost-keeper.exe')
        )
        foreach ($pair in $assets) {
            $asset = $pair[0]
            $target = Join-Path $stage $pair[1]
            Write-Host ">> fetching $asset from $tag"
            Invoke-WebRequest -UseBasicParsing -Uri "$downloads/$tag/$asset" -OutFile $target
            $sidecar = (Invoke-WebRequest -UseBasicParsing -Uri "$downloads/$tag/$asset.sha256").Content
            if ($sidecar -is [byte[]]) { $sidecar = [Text.Encoding]::UTF8.GetString($sidecar) }
            $want = (($sidecar -split '\s+') | Where-Object { $_ } | Select-Object -First 1)
            if (-not $want) {
                throw "The digest published beside $asset is not a digest. Nothing was installed."
            }
            $got = (Get-FileHash -Algorithm SHA256 -Path $target).Hash.ToLower()
            if ($got -ne $want.ToLower()) {
                throw "The $tag asset $asset does not match the digest published beside it (expected $want, actual $got). Nothing was installed."
            }
            Write-Host ">> $asset matches the digest published beside it"
        }

        & (Join-Path $stage 'roost.exe') join
        $code = $LASTEXITCODE
        if ($code -eq 0) {
            & (Join-Path $stage 'roost.exe') self-link
            if ($LASTEXITCODE -ne 0) {
                Write-Warning 'roost self-link did not write the roost shim; run it again from the installed release.'
            }
            $bin = Join-Path $env:LOCALAPPDATA 'RoostWorkerV3\bin'
            $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
            $entries = @($userPath -split ';' | Where-Object { $_ })
            if (-not ($entries | Where-Object { $_.TrimEnd('\') -ieq $bin })) {
                [Environment]::SetEnvironmentVariable('Path', (($entries + $bin) -join ';'), 'User')
                $env:Path = "$env:Path;$bin"
                Write-Host ">> added $bin to your Path; open a new terminal to run roost"
            }
        }
    }
    finally {
        Remove-Item -Recurse -Force -Path $stage -ErrorAction SilentlyContinue
    }
    $global:LASTEXITCODE = $code
    if ($code -ne 0) {
        # `throw`, not `exit`: under `irm | iex` an exit closes the operator's
        # own PowerShell window along with the message.
        throw "roost join exited $code"
    }
}

# The tag to install, by the same rule as install.sh's `install_tag`: the
# highest v3 tag of any kind under ROOST_RELEASE_CHANNEL=prerelease, otherwise
# the highest stable one, and only while no stable v3 exists and no channel was
# named, the highest pre-release. Versions order by a key whose numbers are
# zero-padded to ten digits and whose stable releases end in `~`, which sorts
# after every `-…`, compared ordinally.
function Select-RoostTag {
    param([string] $Api, [string] $Channel)
    $releases = Invoke-RestMethod -UseBasicParsing -Uri $Api -Headers @{ Accept = 'application/vnd.github+json' }
    $ranked = New-Object System.Collections.Generic.List[object]
    foreach ($release in $releases) {
        $tag = [string] $release.tag_name
        if ($tag -notmatch '^v3\.') { continue }
        $version = ($tag.Substring(1) -split '\+', 2)[0]
        $pre = ''
        $dash = $version.IndexOf('-')
        if ($dash -ge 0) {
            $pre = $version.Substring($dash + 1)
            $version = $version.Substring(0, $dash)
        }
        $core = $version -split '\.'
        if ($core.Count -ne 3 -or ($core | Where-Object { $_ -notmatch '^[0-9]+$' })) { continue }
        $key = '{0:D10}.{1:D10}.{2:D10}' -f [long] $core[0], [long] $core[1], [long] $core[2]
        if ($pre -eq '') {
            $ranked.Add([pscustomobject] @{ Key = "$key~"; Stable = $true; Tag = $tag })
            continue
        }
        $ids = foreach ($id in ($pre -split '\.')) {
            if ($id -match '^[0-9]+$') { '{0:D10}' -f [long] $id } else { $id }
        }
        $ranked.Add([pscustomobject] @{ Key = "$key-" + ($ids -join '.'); Stable = $false; Tag = $tag })
    }
    if ($ranked.Count -eq 0) { return $null }
    $ordered = [object[]] $ranked.ToArray()
    [Array]::Sort([string[]] ($ordered | ForEach-Object { $_.Key }), $ordered, [StringComparer]::Ordinal)
    if ($Channel -eq 'prerelease') {
        return $ordered[-1].Tag
    }
    $stable = @($ordered | Where-Object { $_.Stable })
    if ($stable.Count -gt 0) {
        return $stable[-1].Tag
    }
    if (-not $Channel) {
        Write-Host '>> no stable v3 release yet; installing the newest pre-release'
        return $ordered[-1].Tag
    }
    Write-Host '>> ROOST_RELEASE_CHANNEL=stable and no stable v3 release is published'
    return $null
}

Install-RoostWorker

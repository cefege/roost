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
# read: ROOST_RELEASE_API_URL, ROOST_RELEASE_COMPARE_URL, ROOST_RELEASE_BASE_URL,
# ROOST_RELEASE_CHANNEL. A v3-prefixed tag name is not proof of a v3 build, so a
# candidate is installed only when GitHub's compare endpoint reports its commit
# on v3 (see `Test-RoostTagOnV3`).

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
        $core = (($tag -replace '^[vV]', '') -split '[-+]', 2)[0] -split '\.'
        if (([long]$core[0] -gt 3) -or
            ([long]$core[0] -eq 3 -and [long]$core[1] -gt 0) -or
            ([long]$core[0] -eq 3 -and [long]$core[1] -eq 0 -and [long]$core[2] -ge 5)) {
            $assets += ,@('roost-omp-windows-x64.exe', 'omp.exe')
        }
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
# the highest stable one, and only while no admitted stable v3 exists and no
# channel was named, the highest admitted pre-release. Versions order by a key
# whose numbers are zero-padded to ten digits and whose stable releases end in
# `~`, which sorts after every `-…`, compared ordinally. Candidates are tried
# highest-version first and each must be cut from v3 (see `Test-RoostTagOnV3`),
# so a `v3.`-prefixed tag from a side branch is passed over rather than
# installed; a compare answer GitHub cannot give admits the candidate.
#
# The compare origin is overridable by ROOST_RELEASE_COMPARE_URL for the same
# reason the listing and downloads are: a mirror pins one origin, not three.
function Select-RoostTag {
    param([string] $Api, [string] $Channel)
    $compare = if ($env:ROOST_RELEASE_COMPARE_URL) { $env:ROOST_RELEASE_COMPARE_URL } else { 'https://api.github.com/repos/cefege/roost/compare' }
    # api.github.com refuses a request without a user agent, and the refusal
    # reads exactly like an empty listing or an unreadable compare answer.
    $userAgent = 'roost-install.ps1 (+https://github.com/cefege/roost)'
    $releases = Invoke-RestMethod -UseBasicParsing -Uri $Api -Headers @{ Accept = 'application/vnd.github+json' } -UserAgent $userAgent
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
    # Sorted through the list's own method: `[Array]::Sort(keys, items)` binds
    # the items to a converted copy in Windows PowerShell, leaving them unsorted.
    $ranked.Sort([System.Comparison[object]] { param($left, $right) [string]::CompareOrdinal($left.Key, $right.Key) })
    $ordered = $ranked.ToArray()
    if ($Channel -eq 'prerelease') {
        return Select-RoostHighestTag -Ranked $ordered -Only '' -Compare $compare -UserAgent $userAgent
    }
    if ($Channel -eq 'stable' -and -not ($ordered | Where-Object { $_.Stable })) {
        Write-Host '>> ROOST_RELEASE_CHANNEL=stable and no stable v3 release is published'
        return $null
    }
    if ($Channel -eq 'stable') {
        return Select-RoostHighestTag -Ranked $ordered -Only 'stable' -Compare $compare -UserAgent $userAgent
    }
    $stable = Select-RoostHighestTag -Ranked $ordered -Only 'stable' -Compare $compare -UserAgent $userAgent
    if ($stable) { return $stable }
    Write-Host '>> no stable v3 release yet; installing the newest pre-release'
    return Select-RoostHighestTag -Ranked $ordered -Only 'prerelease' -Compare $compare -UserAgent $userAgent
}

# The highest-version candidate `Only` admits that is cut from v3, walking a
# version-ordered listing from its top. `Only` is 'stable', 'prerelease', or ''
# for either kind; restricting the walk is what keeps the unnamed-channel
# fallback from asking GitHub about a stable tag it already rejected.
function Select-RoostHighestTag {
    param([object[]] $Ranked, [string] $Only, [string] $Compare, [string] $UserAgent)
    for ($idx = $Ranked.Count - 1; $idx -ge 0; $idx--) {
        $candidate = $Ranked[$idx]
        if ($Only -eq 'stable' -and -not $candidate.Stable) { continue }
        if ($Only -eq 'prerelease' -and $candidate.Stable) { continue }
        if (Test-RoostTagOnV3 -Compare $Compare -Tag $candidate.Tag -UserAgent $UserAgent) {
            return $candidate.Tag
        }
    }
    return $null
}

# Whether to install a published tag, by the one question a tag name cannot
# answer: GitHub's compare endpoint reports `identical` or `behind` for a tag
# cut from v3, including every release of a fleet that rolled back, and `ahead`
# or `diverged` for one carrying work v3 does not contain. An answer GitHub
# cannot give admits the candidate, so a rate limit or a mirror does not refuse
# every install; it says so, because an unreachable compare otherwise reads
# exactly like an on-branch tag.
function Test-RoostTagOnV3 {
    param([string] $Compare, [string] $Tag, [string] $UserAgent)
    $status = ''
    try {
        $answer = Invoke-RestMethod -UseBasicParsing -ErrorAction Stop -Uri "$Compare/v3...$Tag" -Headers @{ Accept = 'application/vnd.github+json' } -UserAgent $UserAgent
        $status = [string] $answer.status
    }
    catch {
        $status = ''
    }
    if ($status -eq 'identical' -or $status -eq 'behind') { return $true }
    if ($status -eq 'ahead' -or $status -eq 'diverged') {
        Write-Host ">> skipping ${Tag}: GitHub says its commit is not on v3"
        return $false
    }
    Write-Warning "could not ask GitHub whether $Tag is cut from v3; installing it"
    return $true
}

Install-RoostWorker

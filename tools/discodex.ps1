[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [ValidateSet('start', 'stop', 'restart', 'status', 'deploy', 'install')]
    [string]$Action = 'status',

    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\Discodex'),

    [switch]$Wait,
    [switch]$SkipPath,
    [switch]$NoStart
)

$ErrorActionPreference = 'Stop'

# Resolve relative InstallDir against the caller's filesystem location once up front.
$InstallDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($InstallDir)

$ScriptDir = $PSScriptRoot
$RepoCandidate = Split-Path -Parent $ScriptDir
$RepoRoot = if (Test-Path -LiteralPath (Join-Path $RepoCandidate 'Cargo.toml') -PathType Leaf) {
    $RepoCandidate
} else {
    $null
}
$InstalledExe = Join-Path $InstallDir 'discodex.exe'
$ShortcutPath = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Discodex.lnk'
$LogPath = Join-Path $env:APPDATA 'Discodex\discodex.log'
$CurrentSessionId = (Get-Process -Id $PID).SessionId
$TrayPidFile = Join-Path (Join-Path $env:LOCALAPPDATA 'Discodex') "tray-session-$CurrentSessionId.pid"

function Normalize-PathEntry {
    param([string]$PathValue)
    if ([string]::IsNullOrWhiteSpace($PathValue)) {
        return ''
    }
    try {
        return [System.IO.Path]::GetFullPath($PathValue.Trim()).TrimEnd('\', '/')
    } catch {
        return $PathValue.Trim().TrimEnd('\', '/')
    }
}

function Copy-IfDifferent {
    param(
        [string]$Source,
        [string]$Destination
    )
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
        return
    }
    $normSrc = Normalize-PathEntry $Source
    $normDst = Normalize-PathEntry $Destination
    if ($normSrc.Equals($normDst, [System.StringComparison]::OrdinalIgnoreCase)) {
        return
    }
    $parent = Split-Path -Parent $Destination
    if (-not [string]::IsNullOrWhiteSpace($parent)) {
        New-Item -ItemType Directory -Force -Path $parent | Out-Null
    }
    Copy-Item -LiteralPath $Source -Destination $Destination -Force
}

function Broadcast-EnvironmentChange {
    try {
        if (-not ('Discodex.NativeEnvBroadcast' -as [type])) {
            Add-Type -Namespace 'Discodex' -Name 'NativeEnvBroadcast' -MemberDefinition @'
[System.Runtime.InteropServices.DllImport("user32.dll", SetLastError = true, CharSet = System.Runtime.InteropServices.CharSet.Auto)]
public static extern System.IntPtr SendMessageTimeout(
    System.IntPtr hWnd,
    uint Msg,
    System.UIntPtr wParam,
    string lParam,
    uint fuFlags,
    uint uTimeout,
    out System.UIntPtr lpdwResult
);
'@ | Out-Null
        }
        $result = [System.UIntPtr]::Zero
        [void][Discodex.NativeEnvBroadcast]::SendMessageTimeout(
            [System.IntPtr](0xffff),
            0x001A,
            [System.UIntPtr]::Zero,
            'Environment',
            0x0002,
            2000,
            [ref]$result
        )
    } catch {
        Write-Verbose "Environment broadcast skipped: $($_.Exception.Message)"
    }
}

function Get-DiscodexTrayProcesses {
    $seenIds = @{}
    $results = New-Object System.Collections.Generic.List[object]
    $cimById = @{}
    $cimProcs = @(Get-CimInstance Win32_Process -Filter "Name = 'discodex.exe' AND SessionId = $CurrentSessionId" -ErrorAction SilentlyContinue)
    foreach ($cim in $cimProcs) {
        $cimById[[int]$cim.ProcessId] = $cim
    }

    if (Test-Path -LiteralPath $TrayPidFile -PathType Leaf) {
        $rawPid = (Get-Content -LiteralPath $TrayPidFile -Raw -ErrorAction SilentlyContinue)
        $parsedPid = 0
        if (-not [string]::IsNullOrWhiteSpace($rawPid) -and [int]::TryParse($rawPid.Trim(), [ref]$parsedPid)) {
            $proc = Get-Process -Id $parsedPid -ErrorAction SilentlyContinue
            if ($null -ne $proc -and $proc.SessionId -eq $CurrentSessionId -and $proc.ProcessName -eq 'discodex') {
                $exePath = if ($cimById.ContainsKey($parsedPid)) { [string]$cimById[$parsedPid].ExecutablePath } else { '' }
                if ([string]::IsNullOrWhiteSpace($exePath)) {
                    $exePath = try { [string]$proc.Path } catch { '' }
                }
                $seenIds[$proc.Id] = $true
                $results.Add([pscustomobject]@{
                    Id = $proc.Id
                    ExecutablePath = $exePath
                    Process = $proc
                })
            }
        }
    }

    foreach ($cim in $cimProcs) {
        $procId = [int]$cim.ProcessId
        if ($seenIds.ContainsKey($procId)) {
            continue
        }
        $cmdLine = [string]$cim.CommandLine
        # Exclude standalone dashboard/CLI modes so they neither block start nor get force-stopped.
        if ($cmdLine -match '(^|\s)--(usage|diagnostics|quit|stop)(\s|$)') {
            continue
        }
        $proc = Get-Process -Id $procId -ErrorAction SilentlyContinue
        if ($null -ne $proc -and $proc.SessionId -eq $CurrentSessionId) {
            $seenIds[$procId] = $true
            $results.Add([pscustomobject]@{
                Id = $procId
                ExecutablePath = [string]$cim.ExecutablePath
                Process = $proc
            })
        }
    }

    return ,$results.ToArray()
}

function Test-TrayReady {
    param([int]$ProcessId)
    $ev = $null
    try {
        if ([System.Threading.EventWaitHandle]::TryOpenExisting("Local\Discodex.TrayReady.$ProcessId", [ref]$ev)) {
            try {
                return $ev.WaitOne(0)
            } finally {
                $ev.Dispose()
            }
        }
    } catch {
        Write-Verbose "Tray readiness check failed for PID ${ProcessId}: $($_.Exception.Message)"
    }
    return $false
}

function Get-UserPathRegistryInfo {
    $envKey = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $false)
    if ($null -eq $envKey) {
        return [pscustomobject]@{
            RawValue = ''
            Kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
        }
    }
    try {
        $raw = [string]$envKey.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
        try {
            $kind = $envKey.GetValueKind('Path')
        } catch {
            $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
        }
        return [pscustomobject]@{
            RawValue = $raw
            Kind = $kind
        }
    } finally {
        $envKey.Dispose()
    }
}

function Test-UserPathContains {
    param([string]$TargetDir)
    $normTarget = Normalize-PathEntry $TargetDir
    $info = Get-UserPathRegistryInfo
    if ([string]::IsNullOrWhiteSpace($info.RawValue)) {
        return $false
    }
    foreach ($entry in ($info.RawValue -split ';')) {
        if ([string]::IsNullOrWhiteSpace($entry)) {
            continue
        }
        $expanded = [Environment]::ExpandEnvironmentVariables($entry)
        if ((Normalize-PathEntry $expanded).Equals($normTarget, [System.StringComparison]::OrdinalIgnoreCase)) {
            return $true
        }
    }
    return $false
}

function Resolve-QuitExecutable {
    param([object[]]$RunningProcesses)
    if (Test-Path -LiteralPath $InstalledExe -PathType Leaf) {
        return $InstalledExe
    }
    if ($null -ne $RepoRoot) {
        $repoRelease = Join-Path $RepoRoot 'target\release\discodex.exe'
        if (Test-Path -LiteralPath $repoRelease -PathType Leaf) {
            return $repoRelease
        }
    }
    foreach ($proc in $RunningProcesses) {
        if (-not [string]::IsNullOrWhiteSpace($proc.ExecutablePath) -and (Test-Path -LiteralPath $proc.ExecutablePath -PathType Leaf)) {
            return $proc.ExecutablePath
        }
    }
    return $null
}

function Stop-Discodex {
    $procs = @(Get-DiscodexTrayProcesses)
    if ($procs.Count -eq 0) {
        Write-Output 'Discodex is not running.'
        return
    }

    $quitExe = Resolve-QuitExecutable -RunningProcesses $procs
    if (-not [string]::IsNullOrWhiteSpace($quitExe)) {
        try {
            Start-Process -FilePath $quitExe -ArgumentList '--quit' -Wait -ErrorAction Stop
        } catch {
            Write-Verbose "Graceful --quit failed: $($_.Exception.Message)"
        }
    }

    for ($i = 0; $i -lt 20; $i++) {
        if (@(Get-DiscodexTrayProcesses).Count -eq 0) {
            break
        }
        Start-Sleep -Milliseconds 100
    }

    $remaining = @(Get-DiscodexTrayProcesses)
    if ($remaining.Count -gt 0) {
        $remaining | ForEach-Object { $_.Process } | Stop-Process -Force -ErrorAction SilentlyContinue
        for ($i = 0; $i -lt 20; $i++) {
            if (@(Get-DiscodexTrayProcesses).Count -eq 0) {
                break
            }
            Start-Sleep -Milliseconds 150
        }
    }

    if (@(Get-DiscodexTrayProcesses).Count -gt 0) {
        throw 'Discodex process remained running after stop attempt.'
    }
    Write-Output 'Discodex stopped.'
}

function Resolve-LaunchExecutable {
    if (Test-Path -LiteralPath $InstalledExe -PathType Leaf) {
        return $InstalledExe
    }
    if ($null -ne $RepoRoot) {
        $repoRelease = Join-Path $RepoRoot 'target\release\discodex.exe'
        if (Test-Path -LiteralPath $repoRelease -PathType Leaf) {
            return $repoRelease
        }
    }
    throw "Discodex executable not found at '$InstalledExe'. Run 'tools\discodex.ps1 deploy' from the repository first."
}

function Start-Discodex {
    $existing = @(Get-DiscodexTrayProcesses)
    if ($existing.Count -gt 0) {
        $pids = ($existing | ForEach-Object { $_.Id }) -join ', '
        Write-Output "Discodex is already running (PID $pids)."
        return
    }

    $exe = Resolve-LaunchExecutable
    $workDir = Split-Path -Parent $exe
    if ($Wait) {
        Start-Process -FilePath $exe -WorkingDirectory $workDir -Wait
        return
    }

    $proc = Start-Process -FilePath $exe -WorkingDirectory $workDir -PassThru
    $started = @()
    $ready = $false
    for ($i = 0; $i -lt 20; $i++) {
        $started = @(Get-DiscodexTrayProcesses)
        foreach ($candidate in $started) {
            if (Test-TrayReady -ProcessId $candidate.Id) {
                $ready = $true
                break
            }
        }
        if ($ready) {
            break
        }
        if ($null -ne $proc -and $proc.HasExited) {
            break
        }
        Start-Sleep -Milliseconds 100
    }

    if ($ready) {
        $pids = ($started | ForEach-Object { $_.Id }) -join ', '
        Write-Output "Discodex started (PID $pids)."
    } elseif ($started.Count -gt 0) {
        $pids = ($started | ForEach-Object { $_.Id }) -join ', '
        Write-Output "Discodex launched (PID $pids); tray readiness not confirmed yet."
    } else {
        Write-Output "Discodex launched from '$exe'."
    }
}

function Install-DiscodexIntegration {
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

    Copy-IfDifferent -Source $PSCommandPath -Destination (Join-Path $InstallDir 'discodex.ps1')
    $startScript = Join-Path $ScriptDir 'start-discodex.ps1'
    Copy-IfDifferent -Source $startScript -Destination (Join-Path $InstallDir 'start-discodex.ps1')

    $shortcutDir = Split-Path -Parent $ShortcutPath
    New-Item -ItemType Directory -Force -Path $shortcutDir | Out-Null
    $wsh = New-Object -ComObject WScript.Shell
    $lnk = $wsh.CreateShortcut($ShortcutPath)
    $lnk.TargetPath = $InstalledExe
    $lnk.WorkingDirectory = $InstallDir
    $lnk.IconLocation = "$InstalledExe,0"
    $lnk.Description = 'Discodex — AI Discord Rich Presence'
    $lnk.Save()
    Write-Output "Start Menu shortcut updated: $ShortcutPath"

    if (-not $SkipPath) {
        if (-not (Test-UserPathContains -TargetDir $InstallDir)) {
            $pathInfo = Get-UserPathRegistryInfo
            $entries = @()
            if (-not [string]::IsNullOrWhiteSpace($pathInfo.RawValue)) {
                $entries = @($pathInfo.RawValue -split ';' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
            }
            $entries += $InstallDir
            $newRawPath = $entries -join ';'

            $envKey = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
            try {
                # Preserve the existing registry value kind (e.g. REG_EXPAND_SZ) and raw %VAR% tokens.
                $envKey.SetValue('Path', $newRawPath, $pathInfo.Kind)
            } finally {
                $envKey.Dispose()
            }
            Broadcast-EnvironmentChange
            Write-Output "Added '$InstallDir' to User PATH."
        } else {
            Write-Output "User PATH already contains '$InstallDir'."
        }

        $normInstall = Normalize-PathEntry $InstallDir
        $sessionHas = $false
        foreach ($entry in ($env:Path -split ';')) {
            if (-not [string]::IsNullOrWhiteSpace($entry) -and
                (Normalize-PathEntry $entry).Equals($normInstall, [System.StringComparison]::OrdinalIgnoreCase)) {
                $sessionHas = $true
                break
            }
        }
        if (-not $sessionHas) {
            $env:Path = if ([string]::IsNullOrWhiteSpace($env:Path)) { $InstallDir } else { "$env:Path;$InstallDir" }
        }
    }
}

function Resolve-BuildTargetDir {
    if ([string]::IsNullOrWhiteSpace($env:CARGO_TARGET_DIR)) {
        return Join-Path $env:TEMP 'discodex-target'
    }
    if ([System.IO.Path]::IsPathRooted($env:CARGO_TARGET_DIR)) {
        return [System.IO.Path]::GetFullPath($env:CARGO_TARGET_DIR)
    }
    return [System.IO.Path]::GetFullPath((Join-Path $RepoRoot $env:CARGO_TARGET_DIR))
}

function Deploy-Discodex {
    if ($null -eq $RepoRoot) {
        throw 'deploy requires running from a Discodex repository checkout (Cargo.toml not found).'
    }

    $targetDir = Resolve-BuildTargetDir
    # Build into a staging directory when the target release binary is currently in use by a running
    # tray so a failed build leaves the existing tray undisturbed and never fails on file locks.
    $candidateBuiltExe = Join-Path $targetDir 'release\discodex.exe'
    $normCandidate = Normalize-PathEntry $candidateBuiltExe
    foreach ($proc in @(Get-DiscodexTrayProcesses)) {
        if (-not [string]::IsNullOrWhiteSpace($proc.ExecutablePath) -and
            (Normalize-PathEntry $proc.ExecutablePath).Equals($normCandidate, [System.StringComparison]::OrdinalIgnoreCase)) {
            $targetDir = Join-Path $env:TEMP 'discodex-deploy-stage'
            break
        }
    }

    Push-Location $RepoRoot
    try {
        & cargo build --release --target-dir $targetDir
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build --release failed with exit code $LASTEXITCODE"
        }
    } finally {
        Pop-Location
    }

    $builtExe = Join-Path $targetDir 'release\discodex.exe'
    if (-not (Test-Path -LiteralPath $builtExe -PathType Leaf)) {
        throw "Built binary not found at '$builtExe'."
    }

    Stop-Discodex

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Copy-Item -LiteralPath $builtExe -Destination $InstalledExe -Force
    Write-Output "Deployed '$InstalledExe'."

    $repoReleaseDir = Join-Path $RepoRoot 'target\release'
    $repoReleaseExe = Join-Path $repoReleaseDir 'discodex.exe'
    try {
        if (-not (Test-Path -LiteralPath $repoReleaseDir -PathType Container)) {
            New-Item -ItemType Directory -Force -Path $repoReleaseDir | Out-Null
        }
        Copy-Item -LiteralPath $builtExe -Destination $repoReleaseExe -Force
    } catch {
        Write-Verbose "Skipped updating repo target\release\discodex.exe: $($_.Exception.Message)"
    }

    Install-DiscodexIntegration

    if (-not $NoStart) {
        Start-Discodex
    }
}

function Show-DiscodexStatus {
    $running = @(Get-DiscodexTrayProcesses)
    if ($running.Count -gt 0) {
        foreach ($proc in $running) {
            $exePath = if ([string]::IsNullOrWhiteSpace($proc.ExecutablePath)) { 'Unknown path' } else { $proc.ExecutablePath }
            $readyLabel = if (Test-TrayReady -ProcessId $proc.Id) { 'ready' } else { 'starting' }
            Write-Output "Process: Running (PID $($proc.Id), $readyLabel) - $exePath"
        }
    } else {
        Write-Output 'Process: Stopped'
    }

    if (Test-Path -LiteralPath $InstalledExe -PathType Leaf) {
        $item = Get-Item -LiteralPath $InstalledExe
        Write-Output "Installed binary: $($item.FullName) ($($item.Length) bytes, $($item.LastWriteTime))"
    } else {
        Write-Output "Installed binary: Missing ($InstalledExe)"
    }

    Write-Output "Start Menu shortcut: $(if (Test-Path -LiteralPath $ShortcutPath -PathType Leaf) { "Present ($ShortcutPath)" } else { "Missing ($ShortcutPath)" })"
    Write-Output "User PATH entry: $(if (Test-UserPathContains -TargetDir $InstallDir) { "Present ($InstallDir)" } else { "Missing ($InstallDir)" })"

    if (Test-Path -LiteralPath $LogPath -PathType Leaf) {
        $latestPresence = Get-Content -LiteralPath $LogPath -Tail 100 -ErrorAction SilentlyContinue |
            Where-Object { $_ -like '*desired presence changed:*' } |
            Select-Object -Last 1
        if ($latestPresence) {
            Write-Output "Latest presence: $latestPresence"
        }
    }
}

switch ($Action) {
    'start' { Start-Discodex }
    'stop' { Stop-Discodex }
    'restart' {
        Stop-Discodex
        Start-Discodex
    }
    'status' { Show-DiscodexStatus }
    'install' { Install-DiscodexIntegration }
    'deploy' { Deploy-Discodex }
}

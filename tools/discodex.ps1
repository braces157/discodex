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

function Test-UserPathContains {
    param([string]$TargetDir)
    $normTarget = Normalize-PathEntry $TargetDir
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ([string]::IsNullOrWhiteSpace($userPath)) {
        return $false
    }
    foreach ($entry in ($userPath -split ';')) {
        if (-not [string]::IsNullOrWhiteSpace($entry) -and
            (Normalize-PathEntry $entry).Equals($normTarget, [System.StringComparison]::OrdinalIgnoreCase)) {
            return $true
        }
    }
    return $false
}

function Stop-Discodex {
    $procs = @(Get-Process -Name 'discodex' -ErrorAction SilentlyContinue)
    if ($procs.Count -eq 0) {
        Write-Output 'Discodex is not running.'
        return
    }

    if (Test-Path -LiteralPath $InstalledExe -PathType Leaf) {
        try {
            Start-Process -FilePath $InstalledExe -ArgumentList '--quit' -Wait -ErrorAction Stop
        } catch {
            Write-Verbose "Graceful --quit failed: $($_.Exception.Message)"
        }
    }

    for ($i = 0; $i -lt 20; $i++) {
        if (-not (Get-Process -Name 'discodex' -ErrorAction SilentlyContinue)) {
            break
        }
        Start-Sleep -Milliseconds 100
    }

    $remaining = @(Get-Process -Name 'discodex' -ErrorAction SilentlyContinue)
    if ($remaining.Count -gt 0) {
        Stop-Process -Name 'discodex' -Force -ErrorAction SilentlyContinue
        for ($i = 0; $i -lt 20; $i++) {
            if (-not (Get-Process -Name 'discodex' -ErrorAction SilentlyContinue)) {
                break
            }
            Start-Sleep -Milliseconds 150
        }
    }

    if (Get-Process -Name 'discodex' -ErrorAction SilentlyContinue) {
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
    $existing = @(Get-Process -Name 'discodex' -ErrorAction SilentlyContinue)
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

    Start-Process -FilePath $exe -WorkingDirectory $workDir
    $started = @()
    for ($i = 0; $i -lt 20; $i++) {
        $started = @(Get-Process -Name 'discodex' -ErrorAction SilentlyContinue)
        if ($started.Count -gt 0) {
            break
        }
        Start-Sleep -Milliseconds 100
    }

    if ($started.Count -gt 0) {
        $pids = ($started | ForEach-Object { $_.Id }) -join ', '
        Write-Output "Discodex started (PID $pids)."
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
            $currentUserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
            $entries = @()
            if (-not [string]::IsNullOrWhiteSpace($currentUserPath)) {
                $entries = @($currentUserPath -split ';' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
            }
            $entries += $InstallDir
            $newUserPath = $entries -join ';'
            [Environment]::SetEnvironmentVariable('Path', $newUserPath, 'User')
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

function Deploy-Discodex {
    if ($null -eq $RepoRoot) {
        throw 'deploy requires running from a Discodex repository checkout (Cargo.toml not found).'
    }

    $targetDir = if ([string]::IsNullOrWhiteSpace($env:CARGO_TARGET_DIR)) {
        Join-Path $env:TEMP 'discodex-target'
    } else {
        $env:CARGO_TARGET_DIR
    }
    $env:CARGO_TARGET_DIR = $targetDir

    Push-Location $RepoRoot
    try {
        & cargo build --release
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
    $running = @(Get-CimInstance Win32_Process -Filter "Name like '%discodex%'" -ErrorAction SilentlyContinue)
    if ($running.Count -gt 0) {
        foreach ($proc in $running) {
            Write-Output "Process: Running (PID $($proc.ProcessId)) - $($proc.ExecutablePath)"
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

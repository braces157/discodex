[CmdletBinding()]
param(
    [string]$ExecutablePath
)

$ErrorActionPreference = 'Stop'

try {
    if ([string]::IsNullOrWhiteSpace($ExecutablePath)) {
        $siblingExecutable = Join-Path $PSScriptRoot 'discodex.exe'
        $ExecutablePath = if (Test-Path -LiteralPath $siblingExecutable -PathType Leaf) {
            $siblingExecutable
        } else {
            Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release\discodex.exe'
        }
    }
    $executable = (Resolve-Path -LiteralPath $ExecutablePath).ProviderPath
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $executable
    $startInfo.WorkingDirectory = Split-Path -Parent $executable
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true

    $process = [System.Diagnostics.Process]::Start($startInfo)
    if ($null -eq $process) {
        throw 'Windows did not start Discodex.'
    }
    $process.Dispose()
} catch {
    Add-Type -AssemblyName System.Windows.Forms
    [System.Windows.Forms.MessageBox]::Show(
        "Could not launch Discodex.`n`n$($_.Exception.Message)",
        'Discodex launcher',
        [System.Windows.Forms.MessageBoxButtons]::OK,
        [System.Windows.Forms.MessageBoxIcon]::Error
    ) | Out-Null
    exit 1
}

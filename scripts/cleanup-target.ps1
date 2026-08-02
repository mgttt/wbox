[CmdletBinding()]
param(
    [string]$TargetDir,
    [switch]$KeepIncremental,
    [switch]$CleanIncremental,
    [switch]$CargoFinished,
    [switch]$RequestStaleDebugCompaction,
    [ValidateRange(1, 1048576)][int]$MaxIncrementalSizeMiB = 512,
    [ValidateRange(1, 1048576)][int]$MaxDebugSizeMiB = 4096,
    [ValidateRange(1, 100)][int]$KeepIncrementalPerCrate = 1
)
$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$runtime = $env:AGENTERM_SCRIPT_EXE
if (-not $runtime) {
    $runtime = @(
        (Join-Path ([Environment]::GetFolderPath('UserProfile')) 'bin\rhai.cmd'),
        'D:\dev\agenterm\dist\agenterm-script.exe',
        'D:\dev\agenterm\target\debug\agenterm-script.exe'
    ) | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
}
if (-not $runtime) { throw "Agenterm Rhai runtime not found; set AGENTERM_SCRIPT_EXE" }
$arguments = @('run', (Join-Path $PSScriptRoot 'rhai\cleanup-target.rhai'),
    '--cwd', $repoRoot, '--project-root', $repoRoot, '--timeout-ms', '3600000', '--')
if ($TargetDir) { $arguments += @('--target-dir', $TargetDir) }
if ($KeepIncremental) { $arguments += '--keep-incremental' }
if ($CleanIncremental) { $arguments += '--clean-incremental' }
if ($CargoFinished) { $arguments += '--cargo-finished' }
& $runtime @arguments
exit $LASTEXITCODE

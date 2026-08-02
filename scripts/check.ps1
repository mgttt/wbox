[CmdletBinding()]
param(
    [switch]$Quick,
    [switch]$WindowsTarget,
    [switch]$CleanIncremental,
    [switch]$KeepIncremental,
    [ValidateRange(1, 1048576)]
    [int]$MaxIncrementalSizeMiB = 512,
    [ValidateRange(1, 1048576)]
    [int]$MaxDebugSizeMiB = 4096,
    [ValidateRange(1, 100)]
    [int]$KeepIncrementalPerCrate = 1
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
$forward = @()
if ($Quick) { $forward += '--quick' }
if ($WindowsTarget) { $forward += '--windows-target' }
$arguments = @('run', (Join-Path $PSScriptRoot 'rhai\check.rhai'),
    '--cwd', $repoRoot, '--project-root', $repoRoot, '--timeout-ms', '3600000', '--') + $forward
& $runtime @arguments
$checkExit = $LASTEXITCODE
if ($checkExit -eq 0) {
    $cleanup = @('run', (Join-Path $PSScriptRoot 'rhai\cleanup-target.rhai'),
        '--cwd', $repoRoot, '--project-root', $repoRoot, '--timeout-ms', '3600000', '--',
        '--cargo-finished')
    if ($KeepIncremental) { $cleanup += '--keep-incremental' }
    if ($CleanIncremental) { $cleanup += '--clean-incremental' }
    & $runtime @cleanup
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
exit $checkExit

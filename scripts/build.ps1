[CmdletBinding()]
param(
    [switch]$Release,
    [string]$Package,
    [string]$Target,
    [switch]$KeepIncremental,
    [switch]$CleanIncremental,
    [ValidateRange(1, 1048576)][int]$MaxIncrementalSizeMiB = 512,
    [ValidateRange(1, 1048576)][int]$MaxDebugSizeMiB = 4096,
    [ValidateRange(1, 100)][int]$KeepIncrementalPerCrate = 1,
    [string[]]$ExtraArgs = @()
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
if ($Release) { $forward += '--release' }
if ($Package) { $forward += @('--package', $Package) }
if ($Target) { $forward += @('--target', $Target) }
if ($KeepIncremental) { $forward += '--keep-incremental' }
if ($CleanIncremental) { $forward += '--clean-incremental' }
$forward += $ExtraArgs
$arguments = @('run', (Join-Path $PSScriptRoot 'rhai\build.rhai'),
    '--cwd', $repoRoot, '--project-root', $repoRoot, '--timeout-ms', '3600000', '--') + $forward
& $runtime @arguments
$buildExit = $LASTEXITCODE

$cleanupParams = @{
    CargoFinished = $true
    MaxIncrementalSizeMiB = $MaxIncrementalSizeMiB
    MaxDebugSizeMiB = $MaxDebugSizeMiB
    KeepIncrementalPerCrate = $KeepIncrementalPerCrate
}
if ($KeepIncremental) { $cleanupParams.KeepIncremental = $true }
if ($CleanIncremental) { $cleanupParams.CleanIncremental = $true }
$canCompactHostDebug = -not $Release -and -not $Package -and -not $Target -and
    $ExtraArgs.Count -eq 0
if ($buildExit -eq 0 -and $canCompactHostDebug) {
    $cleanupParams.RequestStaleDebugCompaction = $true
}

& (Join-Path $PSScriptRoot 'cleanup-target.ps1') @cleanupParams
$cleanupExit = $LASTEXITCODE
if ($buildExit -ne 0) { exit $buildExit }
if ($cleanupExit -ne 75) { exit $cleanupExit }

# Cargo owns profile invalidation. If cleanup detects stale platform revisions
# in an oversized debug profile, rebuild the profile instead of deleting pieces.
& cargo clean --profile dev
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& $runtime @arguments
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$cleanupParams.Remove('RequestStaleDebugCompaction')
& (Join-Path $PSScriptRoot 'cleanup-target.ps1') @cleanupParams
exit $LASTEXITCODE

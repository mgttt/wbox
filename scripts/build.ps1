[CmdletBinding()]
param(
    [switch]$Release,
    [string]$Package,
    [string]$Target,
    [switch]$KeepIncremental,
    [switch]$CleanIncremental,
    [ValidateRange(1, 1048576)]
    [int]$MaxIncrementalSizeMiB = 512,
    [ValidateRange(1, 1048576)]
    [int]$MaxDebugSizeMiB = 4096,
    [ValidateRange(1, 100)]
    [int]$KeepIncrementalPerCrate = 1,
    [string[]]$ExtraArgs = @()
)

$ErrorActionPreference = "Stop"
$staleDebugCompactionExitCode = 75
if ($KeepIncremental -and $CleanIncremental) {
    throw "-KeepIncremental and -CleanIncremental cannot be used together"
}

$cargoArgs = @("build", "--locked")
if ($Release) {
    $cargoArgs += "--release"
}
if ($Package) {
    $cargoArgs += @("--package", $Package)
}
if ($Target) {
    $cargoArgs += @("--target", $Target)
}
$cargoArgs += $ExtraArgs

$repoRoot = Split-Path -Parent $PSScriptRoot
$buildExit = 0
$cleanupExit = 0
$canCompactHostDebug = -not $Release -and -not $Package -and -not $Target -and
    $ExtraArgs.Count -eq 0
Push-Location $repoRoot
try {
    & cargo @cargoArgs
    $buildExit = $LASTEXITCODE
} finally {
    try {
        & (Join-Path $PSScriptRoot "cleanup-target.ps1") `
            -KeepIncremental:$KeepIncremental `
            -CleanIncremental:$CleanIncremental `
            -CargoFinished `
            -RequestStaleDebugCompaction:$($buildExit -eq 0 -and $canCompactHostDebug) `
            -MaxIncrementalSizeMiB $MaxIncrementalSizeMiB `
            -MaxDebugSizeMiB $MaxDebugSizeMiB `
            -KeepIncrementalPerCrate $KeepIncrementalPerCrate
        $cleanupExit = $LASTEXITCODE
    } finally {
        Pop-Location
    }
}

if ($buildExit -ne 0) {
    exit $buildExit
}
if ($cleanupExit -eq $staleDebugCompactionExitCode) {
    & cargo clean --profile dev
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
    & (Join-Path $PSScriptRoot "cleanup-target.ps1") `
        -KeepIncremental:$KeepIncremental `
        -CleanIncremental:$CleanIncremental `
        -CargoFinished `
        -MaxIncrementalSizeMiB $MaxIncrementalSizeMiB `
        -MaxDebugSizeMiB $MaxDebugSizeMiB `
        -KeepIncrementalPerCrate $KeepIncrementalPerCrate
    exit $LASTEXITCODE
}
exit $cleanupExit

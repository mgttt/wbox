[CmdletBinding()]
param(
    [switch]$Release,
    [string]$Package,
    [string]$Target,
    [switch]$KeepIncremental,
    [switch]$CleanIncremental,
    [string[]]$ExtraArgs = @()
)

$ErrorActionPreference = "Stop"
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
Push-Location $repoRoot
try {
    & cargo @cargoArgs
    $buildExit = $LASTEXITCODE
} finally {
    try {
        & (Join-Path $PSScriptRoot "cleanup-target.ps1") `
            -KeepIncremental:$KeepIncremental `
            -CleanIncremental:$CleanIncremental
    } finally {
        Pop-Location
    }
}

exit $buildExit

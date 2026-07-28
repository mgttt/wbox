[CmdletBinding()]
param(
    [switch]$Release,
    [string]$Package,
    [string]$Target,
    [switch]$KeepIncremental,
    [string[]]$ExtraArgs = @()
)

$ErrorActionPreference = "Stop"

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
        & (Join-Path $PSScriptRoot "cleanup-target.ps1") -KeepIncremental:$KeepIncremental
    } finally {
        Pop-Location
    }
}

exit $buildExit

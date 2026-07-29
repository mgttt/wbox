[CmdletBinding()]
param(
    [switch]$Quick,
    [switch]$WindowsTarget,
    [switch]$KeepIncremental
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$timer = [System.Diagnostics.Stopwatch]::StartNew()
$exitCode = 0

Push-Location $repoRoot
try {
    & (Join-Path $PSScriptRoot "lint.ps1") -Mode All -WindowsTarget:$WindowsTarget
    if ($LASTEXITCODE -ne 0) {
        throw "lint failed with code $LASTEXITCODE"
    }

    $testArgs = @("test", "--locked", "--workspace")
    if ($Quick) {
        $testArgs += "--lib"
    }
    $testArgs += "--quiet"

    & cargo @testArgs
    if ($LASTEXITCODE -ne 0) {
        throw "cargo test failed with code $LASTEXITCODE"
    }
} catch {
    Write-Error $_
    $exitCode = 1
} finally {
    try {
        & (Join-Path $PSScriptRoot "cleanup-target.ps1") -KeepIncremental:$KeepIncremental
    } catch {
        Write-Error "post-check cleanup failed: $_"
        $exitCode = 1
    }
    Pop-Location
}

$timer.Stop()
$modeName = if ($Quick) { "quick" } else { "full Rust" }
Write-Host "check: $modeName completed in $([Math]::Round($timer.Elapsed.TotalSeconds, 2))s (exit=$exitCode)"
exit $exitCode

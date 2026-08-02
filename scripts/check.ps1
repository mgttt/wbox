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
$staleDebugCompactionExitCode = 75
if ($CleanIncremental -and $KeepIncremental) {
    throw "-CleanIncremental and -KeepIncremental cannot be used together"
}
$repoRoot = Split-Path -Parent $PSScriptRoot
$timer = [System.Diagnostics.Stopwatch]::StartNew()
$exitCode = 0
$compactDebug = $false

Push-Location $repoRoot
try {
    & (Join-Path $PSScriptRoot "lint.ps1") -Mode All -WindowsTarget:$WindowsTarget
    if ($LASTEXITCODE -ne 0) {
        throw "lint failed with code $LASTEXITCODE"
    }

    & (Join-Path $PSScriptRoot "test-cleanup-target.ps1")
    if ($LASTEXITCODE -ne 0) {
        throw "target cleanup fixture failed with code $LASTEXITCODE"
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
        & (Join-Path $PSScriptRoot "cleanup-target.ps1") `
            -KeepIncremental:$KeepIncremental `
            -CleanIncremental:$CleanIncremental `
            -CargoFinished `
            -RequestStaleDebugCompaction:$($exitCode -eq 0) `
            -MaxIncrementalSizeMiB $MaxIncrementalSizeMiB `
            -MaxDebugSizeMiB $MaxDebugSizeMiB `
            -KeepIncrementalPerCrate $KeepIncrementalPerCrate
        if ($LASTEXITCODE -eq $staleDebugCompactionExitCode) {
            $compactDebug = $true
        } elseif ($LASTEXITCODE -ne 0) {
            throw "target cleanup failed with code $LASTEXITCODE"
        }
    } catch {
        Write-Error "post-check cleanup failed: $_"
        $exitCode = 1
    }
    Pop-Location
}

if ($compactDebug -and $exitCode -eq 0) {
    Push-Location $repoRoot
    try {
        & cargo clean --profile dev
        if ($LASTEXITCODE -ne 0) {
            throw "cargo clean --profile dev failed with code $LASTEXITCODE"
        }
        & cargo @testArgs
        if ($LASTEXITCODE -ne 0) {
            throw "cargo test after debug compaction failed with code $LASTEXITCODE"
        }
        & (Join-Path $PSScriptRoot "cleanup-target.ps1") `
            -KeepIncremental:$KeepIncremental `
            -CleanIncremental:$CleanIncremental `
            -CargoFinished `
            -MaxIncrementalSizeMiB $MaxIncrementalSizeMiB `
            -MaxDebugSizeMiB $MaxDebugSizeMiB `
            -KeepIncrementalPerCrate $KeepIncrementalPerCrate
        if ($LASTEXITCODE -ne 0) {
            throw "target cleanup after debug compaction failed with code $LASTEXITCODE"
        }
    } catch {
        Write-Error $_
        $exitCode = 1
    } finally {
        Pop-Location
    }
}

$timer.Stop()
$modeName = if ($Quick) { "quick" } else { "full Rust" }
Write-Host "check: $modeName completed in $([Math]::Round($timer.Elapsed.TotalSeconds, 2))s (exit=$exitCode)"
exit $exitCode

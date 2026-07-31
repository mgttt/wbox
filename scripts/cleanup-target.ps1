[CmdletBinding()]
param(
    [string]$TargetDir,
    [switch]$KeepIncremental
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
if (-not $TargetDir) {
    $TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "target" }
}
if (-not [System.IO.Path]::IsPathRooted($TargetDir)) {
    $TargetDir = Join-Path $repoRoot $TargetDir
}
$targetRoot = [System.IO.Path]::GetFullPath($TargetDir)

if (-not (Test-Path -LiteralPath $targetRoot -PathType Container)) {
    Write-Host "target cleanup: nothing to clean at $targetRoot"
    exit 0
}

$cacheTag = Join-Path $targetRoot "CACHEDIR.TAG"
if (-not (Test-Path -LiteralPath $cacheTag -PathType Leaf) -or
    -not (Select-String -LiteralPath $cacheTag -SimpleMatch "created by cargo" -Quiet)) {
    throw "Refusing to clean '$targetRoot': Cargo CACHEDIR.TAG is missing."
}

$targetPrefix = $targetRoot.TrimEnd(
    [System.IO.Path]::DirectorySeparatorChar,
    [System.IO.Path]::AltDirectorySeparatorChar
) + [System.IO.Path]::DirectorySeparatorChar
$removed = 0
$removedFiles = 0

function Remove-TargetDirectory {
    param([Parameter(Mandatory)][System.IO.DirectoryInfo]$Directory)

    $fullPath = [System.IO.Path]::GetFullPath($Directory.FullName)
    if (-not $fullPath.StartsWith($targetPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to clean path outside Cargo target directory: $fullPath"
    }

    if (($Directory.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        Remove-Item -LiteralPath $fullPath -Force
    } else {
        Remove-Item -LiteralPath $fullPath -Recurse -Force
    }
    $script:removed++
}

function Remove-TargetFile {
    param([Parameter(Mandatory)][System.IO.FileInfo]$File)

    $fullPath = [System.IO.Path]::GetFullPath($File.FullName)
    if (-not $fullPath.StartsWith($targetPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to clean file outside Cargo target directory: $fullPath"
    }
    Remove-Item -LiteralPath $fullPath -Force
    $script:removedFiles++
}

if (-not $KeepIncremental) {
    $incrementalDirs = @(
        Get-ChildItem -LiteralPath $targetRoot -Directory -Recurse -Force |
            Where-Object { $_.Name -eq "incremental" }
    )
    foreach ($directory in $incrementalDirs) {
        Remove-TargetDirectory -Directory $directory
    }
}

$temporaryDirs = @(
    Get-ChildItem -LiteralPath $targetRoot -Directory -Force |
        Where-Object { $_.Name -eq "tmp" -or $_.Name -like "review-*" }
)
foreach ($directory in $temporaryDirs) {
    Remove-TargetDirectory -Directory $directory
}

$temporaryFiles = @(
    Get-ChildItem -LiteralPath $targetRoot -File -Force |
        Where-Object {
            $_.Name -like "*.tmp" -or
            $_.Name -like "*.part" -or
            $_.Name -like "review-*"
        }
)
foreach ($file in $temporaryFiles) {
    Remove-TargetFile -File $file
}

Write-Host "target cleanup: removed $removed regenerable director$(if ($removed -eq 1) { 'y' } else { 'ies' }) and $removedFiles temporary file$(if ($removedFiles -eq 1) { '' } else { 's' })"

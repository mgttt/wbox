[CmdletBinding()]
param(
    [string]$TargetDir,
    [switch]$KeepIncremental,
    [switch]$CleanIncremental,
    [ValidateRange(1, 1048576)]
    [int]$MaxIncrementalSizeMiB = 512,
    [ValidateRange(1, 100)]
    [int]$KeepIncrementalPerCrate = 1
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
$removedBytes = 0L

function Remove-TargetDirectory {
    param([Parameter(Mandatory)][System.IO.DirectoryInfo]$Directory)

    $fullPath = [System.IO.Path]::GetFullPath($Directory.FullName)
    if (-not $fullPath.StartsWith($targetPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to clean path outside Cargo target directory: $fullPath"
    }

    if (($Directory.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        Remove-Item -LiteralPath $fullPath -Force
    } else {
        $bytes = (Get-ChildItem -LiteralPath $fullPath -File -Recurse -Force -ErrorAction SilentlyContinue |
            Measure-Object -Property Length -Sum).Sum
        if ($null -ne $bytes) {
            $script:removedBytes += [long]$bytes
        }
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
    $script:removedBytes += $File.Length
    Remove-Item -LiteralPath $fullPath -Force
    $script:removedFiles++
}

function Remove-SupersededIncrementalSessions {
    param([Parameter(Mandatory)][System.IO.DirectoryInfo]$CrateDirectory)

    $lockFiles = @(
        Get-ChildItem -LiteralPath $CrateDirectory.FullName -File -Filter "s-*.lock" -Force
    )
    $sessions = @(
        foreach ($sessionDirectory in @(
            Get-ChildItem -LiteralPath $CrateDirectory.FullName -Directory -Force |
                Where-Object { $_.Name -like "s-*-*" -and $_.Name -notlike "*-working" }
        )) {
            $matchingLock = $lockFiles |
                Where-Object {
                    $sessionDirectory.Name.StartsWith(
                        $_.BaseName + "-",
                        [System.StringComparison]::Ordinal
                    )
                } |
                Sort-Object { $_.BaseName.Length } -Descending |
                Select-Object -First 1
            if ($matchingLock) {
                [pscustomobject]@{
                    Directory = $sessionDirectory
                    Lock = $matchingLock
                }
            }
        }
    )

    $sessions |
        Sort-Object { $_.Directory.LastWriteTimeUtc } -Descending |
        Select-Object -Skip 1 |
        ForEach-Object {
            Remove-TargetDirectory -Directory $_.Directory
            if (Test-Path -LiteralPath $_.Lock.FullName -PathType Leaf) {
                Remove-TargetFile -File $_.Lock
            }
        }
}

if ($KeepIncremental -and $CleanIncremental) {
    throw "-KeepIncremental and -CleanIncremental cannot be used together"
}

if (-not $KeepIncremental) {
    $incrementalRoot = Join-Path $targetRoot "debug\incremental"
    if (Test-Path -LiteralPath $incrementalRoot -PathType Container) {
        if ($CleanIncremental) {
            Remove-TargetDirectory -Directory (Get-Item -LiteralPath $incrementalRoot -Force)
        } else {
            # Cargo leaves old session lock files behind after replacing a session.
            # A live/reusable lock has a sibling session directory whose name starts
            # with the lock basename followed by the session metadata hash.
            $crateDirs = @(Get-ChildItem -LiteralPath $incrementalRoot -Directory -Force)
            foreach ($crateDir in $crateDirs) {
                Remove-SupersededIncrementalSessions -CrateDirectory $crateDir
                $sessionDirs = @(Get-ChildItem -LiteralPath $crateDir.FullName -Directory -Force)
                $lockFiles = @(
                    Get-ChildItem -LiteralPath $crateDir.FullName -File -Filter "*.lock" -Force
                )
                foreach ($lockFile in $lockFiles) {
                    $sessionPrefix = $lockFile.BaseName + "-"
                    $hasSession = $sessionDirs | Where-Object {
                        $_.Name.StartsWith($sessionPrefix, [System.StringComparison]::Ordinal)
                    } | Select-Object -First 1
                    if (-not $hasSession) {
                        Remove-TargetFile -File $lockFile
                    }
                }

                if (-not (Get-ChildItem -LiteralPath $crateDir.FullName -Force | Select-Object -First 1)) {
                    Remove-TargetDirectory -Directory $crateDir
                }
            }

            # Cargo keys incremental units by crate plus a compilation hash. Feature,
            # target and test changes therefore leave complete but cold units behind.
            # Bound those units per crate even when the global cache is under budget.
            $crateDirs = @(Get-ChildItem -LiteralPath $incrementalRoot -Directory -Force)
            $incrementalUnits = @(
                foreach ($crateDir in $crateDirs) {
                    $separator = $crateDir.Name.LastIndexOf("-")
                    $crateName = if ($separator -gt 0) {
                        $crateDir.Name.Substring(0, $separator)
                    } else {
                        $crateDir.Name
                    }
                    $bytes = (Get-ChildItem -LiteralPath $crateDir.FullName -File -Recurse -Force `
                            -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
                    [pscustomobject]@{
                        Directory = $crateDir
                        CrateName = $crateName
                        Bytes = if ($null -eq $bytes) { 0L } else { [long]$bytes }
                    }
                }
            )
            foreach ($group in @($incrementalUnits | Group-Object -Property CrateName)) {
                $group.Group |
                    Sort-Object { $_.Directory.LastWriteTimeUtc } -Descending |
                    Select-Object -Skip $KeepIncrementalPerCrate |
                    ForEach-Object { Remove-TargetDirectory -Directory $_.Directory }
            }

            $incrementalUnits = @(
                $incrementalUnits | Where-Object {
                    Test-Path -LiteralPath $_.Directory.FullName -PathType Container
                }
            )
            $incrementalBytes = [long](
                $incrementalUnits | Measure-Object -Property Bytes -Sum
            ).Sum
            $maxIncrementalBytes = [long]$MaxIncrementalSizeMiB * 1MB
            if ($incrementalBytes -gt $maxIncrementalBytes) {
                # Preserve the newest unit for every crate. Second-newest units are
                # then a global LRU tail that can be discarded to meet the budget.
                $protectedPaths = [System.Collections.Generic.HashSet[string]]::new(
                    [System.StringComparer]::OrdinalIgnoreCase
                )
                foreach ($group in @($incrementalUnits | Group-Object -Property CrateName)) {
                    $group.Group |
                        Sort-Object { $_.Directory.LastWriteTimeUtc } -Descending |
                        Select-Object -First 1 |
                        ForEach-Object { [void]$protectedPaths.Add($_.Directory.FullName) }
                }
                $incrementalUnits |
                    Where-Object { -not $protectedPaths.Contains($_.Directory.FullName) } |
                    Sort-Object { $_.Directory.LastWriteTimeUtc } |
                    ForEach-Object {
                        if ($incrementalBytes -gt $maxIncrementalBytes) {
                            $incrementalBytes -= $_.Bytes
                            Remove-TargetDirectory -Directory $_.Directory
                        }
                    }
            }
        }
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

$released = if ($removedBytes -ge 1GB) {
    "{0:N2} GiB" -f ($removedBytes / 1GB)
} elseif ($removedBytes -ge 1MB) {
    "{0:N1} MiB" -f ($removedBytes / 1MB)
} else {
    "{0:N1} KiB" -f ($removedBytes / 1KB)
}
Write-Host "target cleanup: removed $removed regenerable director$(if ($removed -eq 1) { 'y' } else { 'ies' }) and $removedFiles temporary file$(if ($removedFiles -eq 1) { '' } else { 's' }); released $released"

[CmdletBinding()]
param(
    [string]$TargetDir,
    [switch]$KeepIncremental,
    [switch]$CleanIncremental,
    [switch]$CargoFinished,
    [switch]$RequestStaleDebugCompaction,
    [ValidateRange(1, 1048576)]
    [int]$MaxIncrementalSizeMiB = 512,
    [ValidateRange(1, 1048576)]
    [int]$MaxDebugSizeMiB = 4096,
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
$staleDebugCompactionRequired = $false
$staleDebugCompactionExitCode = 75

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

function Remove-AbandonedWorkingSessions {
    param([Parameter(Mandatory)][System.IO.DirectoryInfo]$CrateDirectory)

    Get-ChildItem -LiteralPath $CrateDirectory.FullName -Directory -Force |
        Where-Object { $_.Name -like "s-*-working" } |
        ForEach-Object { Remove-TargetDirectory -Directory $_ }
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
                if ($CargoFinished) {
                    # The owning Cargo invocation has exited, so its unfinished
                    # sessions cannot be resumed. Standalone cleanup keeps them
                    # in case another Cargo process is still using this target.
                    Remove-AbandonedWorkingSessions -CrateDirectory $crateDir
                }
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

if ($RequestStaleDebugCompaction -and $CargoFinished) {
    # Cargo dep-info retains the exact git checkout revision. Use it only to
    # request a Cargo-coordinated profile rebuild; never infer an artifact
    # dependency closure and delete individual deps/fingerprints here.
    $lockPath = Join-Path $repoRoot "Cargo.lock"
    $debugRoot = Join-Path $targetRoot "debug"
    $depsRoot = Join-Path $debugRoot "deps"
    if ((Test-Path -LiteralPath $lockPath -PathType Leaf) -and
        (Test-Path -LiteralPath $depsRoot -PathType Container)) {
        $lockText = Get-Content -LiteralPath $lockPath -Raw -Encoding UTF8
        $platformRevision = $null
        foreach ($package in [regex]::Matches(
                $lockText,
                '(?ms)^\[\[package\]\]\r?\n(?<body>.*?)(?=^\[\[package\]\]|\z)'
            )) {
            $body = $package.Groups["body"].Value
            if ($body -match '(?m)^name = "agenterm-platform"\s*$' -and
                $body -match '(?m)^source = "git\+[^"#]+(?:\?[^"#]*)?#(?<revision>[0-9a-fA-F]{40})"\s*$') {
                $platformRevision = $Matches["revision"].ToLowerInvariant()
                break
            }
        }

        if ($platformRevision) {
            $staleRevisions = [System.Collections.Generic.HashSet[string]]::new(
                [System.StringComparer]::OrdinalIgnoreCase
            )
            foreach ($depInfo in @(
                    Get-ChildItem -LiteralPath $depsRoot -File `
                        -Filter "agenterm_platform-*.d" -Force
                )) {
                try {
                    $depText = Get-Content -LiteralPath $depInfo.FullName -Raw -ErrorAction Stop
                } catch {
                    continue
                }
                foreach ($checkout in [regex]::Matches(
                        $depText,
                        '[\\/]git[\\/]checkouts[\\/][^\\/]+[\\/](?<revision>[0-9a-fA-F]{7,40})[\\/]crates[\\/]agenterm-platform[\\/]'
                    )) {
                    $revision = $checkout.Groups["revision"].Value.ToLowerInvariant()
                    if (-not $platformRevision.StartsWith(
                            $revision,
                            [System.StringComparison]::OrdinalIgnoreCase
                        )) {
                        [void]$staleRevisions.Add($revision)
                    }
                }
            }

            if ($staleRevisions.Count -gt 0) {
                $debugBytes = (Get-ChildItem -LiteralPath $debugRoot -File -Recurse -Force `
                        -ErrorAction SilentlyContinue |
                    Measure-Object -Property Length -Sum).Sum
                if ($null -eq $debugBytes) {
                    $debugBytes = 0L
                }
                $maxDebugBytes = [long]$MaxDebugSizeMiB * 1MB
                if ([long]$debugBytes -gt $maxDebugBytes) {
                    $staleDebugCompactionRequired = $true
                    $compactionMessage = (
                        "target cleanup: debug is {0:N1} MiB and contains stale " +
                        "agenterm-platform revision(s) {1}; requesting Cargo-coordinated compaction"
                    ) -f ($debugBytes / 1MB), (($staleRevisions | Sort-Object) -join ",")
                    Write-Host $compactionMessage
                }
            }
        }
    }
}

$released = if ($removedBytes -ge 1GB) {
    "{0:N2} GiB" -f ($removedBytes / 1GB)
} elseif ($removedBytes -ge 1MB) {
    "{0:N1} MiB" -f ($removedBytes / 1MB)
} else {
    "{0:N1} KiB" -f ($removedBytes / 1KB)
}
Write-Host "target cleanup: removed $removed regenerable director$(if ($removed -eq 1) { 'y' } else { 'ies' }) and $removedFiles temporary file$(if ($removedFiles -eq 1) { '' } else { 's' }); released $released"
if ($staleDebugCompactionRequired) {
    exit $staleDebugCompactionExitCode
}
exit 0

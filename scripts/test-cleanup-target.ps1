[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$staleDebugCompactionExitCode = 75
$repoRoot = Split-Path -Parent $PSScriptRoot
$manifest = Get-Content -LiteralPath (Join-Path $repoRoot "Cargo.toml") -Raw -Encoding UTF8
$revisionMatch = [regex]::Match(
    $manifest,
    '(?m)^agenterm-platform = \{[^\r\n]*\brev = "(?<revision>[0-9a-fA-F]{40})"'
)
if (-not $revisionMatch.Success) {
    throw "cannot find the pinned agenterm-platform revision in Cargo.toml"
}
$currentRevision = $revisionMatch.Groups["revision"].Value.ToLowerInvariant()
$fixture = Join-Path ([System.IO.Path]::GetTempPath()) (
    "wbox-cleanup-{0}-{1}" -f $PID, [Guid]::NewGuid().ToString("N")
)
$deps = Join-Path $fixture "debug\deps"
$depInfo = Join-Path $deps "agenterm_platform-cleanupfixture.d"
$payload = Join-Path $deps "payload.bin"

function Write-PlatformDepInfo([string]$Revision) {
    $checkout = Join-Path $env:USERPROFILE (
        ".cargo\git\checkouts\agenterm-fixture\{0}\crates\agenterm-platform\src\lib.rs" -f
        $Revision
    )
    [System.IO.File]::WriteAllText($depInfo, "$depInfo`: $checkout")
}

function Invoke-CleanupProbe {
    & (Join-Path $PSScriptRoot "cleanup-target.ps1") `
        -TargetDir $fixture `
        -CargoFinished `
        -RequestStaleDebugCompaction `
        -MaxDebugSizeMiB 1 `
        -KeepIncremental
    return $LASTEXITCODE
}

try {
    New-Item -ItemType Directory -Force -Path $deps | Out-Null
    [System.IO.File]::WriteAllText(
        (Join-Path $fixture "CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55`n# This file is a cache directory tag created by cargo.`n"
    )
    $stream = [System.IO.File]::OpenWrite($payload)
    try {
        $stream.SetLength(2MB)
    } finally {
        $stream.Dispose()
    }

    Write-PlatformDepInfo $currentRevision.Substring(0, 7)
    $currentResult = Invoke-CleanupProbe
    if ($currentResult -ne 0) {
        throw "current agenterm-platform revision requested compaction: rc=$currentResult"
    }

    Write-PlatformDepInfo "0000000"
    $staleResult = Invoke-CleanupProbe
    if ($staleResult -ne $staleDebugCompactionExitCode) {
        throw "stale agenterm-platform revision did not request compaction: rc=$staleResult"
    }
    Write-Host "PASS cleanup target current/stale git revision classification"
} finally {
    $tempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
    $fixtureFull = [System.IO.Path]::GetFullPath($fixture)
    if ($fixtureFull.StartsWith($tempRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        Remove-Item -LiteralPath $fixtureFull -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# The accepted stale probe returns the internal request code. The fixture
# itself passed, so do not leak that code to check.ps1.
exit 0

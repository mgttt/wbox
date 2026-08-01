[CmdletBinding()]
param(
    [ValidateSet(
        "i686-pc-windows-msvc",
        "aarch64-unknown-linux-gnu",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin"
    )]
    [string[]]$Target = @(
        "i686-pc-windows-msvc",
        "aarch64-unknown-linux-gnu",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin"
    )
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$previousIncremental = $env:CARGO_INCREMENTAL

Push-Location $repoRoot
try {
    $installedTargets = @(& rustup target list --installed)
    if ($LASTEXITCODE -ne 0) {
        throw "rustup target list failed with code $LASTEXITCODE"
    }

    foreach ($triple in $Target) {
        if ($installedTargets -notcontains $triple) {
            throw "Rust target '$triple' is not installed; run: rustup target add $triple"
        }

        Write-Host "portable target: $triple"
        $env:CARGO_INCREMENTAL = "0"
        & cargo clippy --locked --workspace --all-targets `
            --target $triple --message-format short -- -D warnings
        if ($LASTEXITCODE -ne 0) {
            throw "portable target '$triple' failed with code $LASTEXITCODE"
        }
    }
} finally {
    if ($null -eq $previousIncremental) {
        Remove-Item Env:CARGO_INCREMENTAL -ErrorAction SilentlyContinue
    } else {
        $env:CARGO_INCREMENTAL = $previousIncremental
    }
    Pop-Location
}

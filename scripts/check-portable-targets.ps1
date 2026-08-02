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
$runtime = $env:AGENTERM_SCRIPT_EXE
if (-not $runtime) {
    $runtime = @(
        (Join-Path ([Environment]::GetFolderPath('UserProfile')) 'bin\rhai.cmd'),
        'D:\dev\agenterm\dist\agenterm-script.exe',
        'D:\dev\agenterm\target\debug\agenterm-script.exe'
    ) | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
}
if (-not $runtime) { throw "Agenterm Rhai runtime not found; set AGENTERM_SCRIPT_EXE" }
$arguments = @('run', (Join-Path $PSScriptRoot 'rhai\check-portable-targets.rhai'),
    '--cwd', $repoRoot, '--project-root', $repoRoot, '--timeout-ms', '3600000', '--') + $Target
& $runtime @arguments
exit $LASTEXITCODE

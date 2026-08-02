[CmdletBinding()]
param(
    [ValidateSet("All", "Static", "Rust")]
    [string]$Mode = "All",
    [switch]$WindowsTarget
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$runtime = $env:AGENTERM_SCRIPT_EXE
if (-not $runtime) {
    $runtimeCandidates = @(
        (Join-Path ([Environment]::GetFolderPath('UserProfile')) 'bin\rhai.cmd'),
        'D:\dev\agenterm\dist\agenterm-script.exe',
        'D:\dev\agenterm\target\debug\agenterm-script.exe'
    )
    $runtime = $runtimeCandidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
}
if (-not $runtime) {
    throw "Agenterm Rhai runtime not found; set AGENTERM_SCRIPT_EXE"
}

$arguments = @(
    'run', (Join-Path $PSScriptRoot 'rhai\lint.rhai'),
    '--cwd', $repoRoot, '--project-root', $repoRoot, '--', $Mode
)
if ($WindowsTarget) { $arguments += '--windows-target' }
& $runtime @arguments
exit $LASTEXITCODE

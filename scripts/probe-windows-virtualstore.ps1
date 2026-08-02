[CmdletBinding()]
param([string]$Wbox = "target/release/wbox.exe")

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
$arguments = @('run', (Join-Path $PSScriptRoot 'rhai\probe-windows-virtualstore.rhai'),
    '--cwd', $repoRoot, '--project-root', $repoRoot, '--timeout-ms', '3600000', '--', $Wbox)
& $runtime @arguments
exit $LASTEXITCODE

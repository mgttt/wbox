[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Wbox,
    [string]$Endpoint = "http://1.1.1.1/cdn-cgi/trace",
    [ValidateRange(5, 120)][int]$TimeoutSeconds = 25
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
$arguments = @('run', (Join-Path $PSScriptRoot 'rhai\test-windows-network.rhai'),
    '--cwd', $repoRoot, '--project-root', $repoRoot, '--timeout-ms', '3600000', '--',
    $Wbox, $Endpoint, $TimeoutSeconds)
& $runtime @arguments
exit $LASTEXITCODE

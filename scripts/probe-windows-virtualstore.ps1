[CmdletBinding()]
param(
    [string]$Wbox = "target/release/wbox.exe"
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$wboxPath = (Resolve-Path -LiteralPath (Join-Path $repoRoot $Wbox)).Path
$source = Join-Path $repoRoot "dev/windows-virtualstore-probe.rs"
$sandbox = Join-Path ([IO.Path]::GetTempPath()) (
    "wbox-virtualstore-{0}-{1}" -f $PID, [Guid]::NewGuid().ToString("N")
)
$probeExe = Join-Path $sandbox "windows-virtualstore-probe.exe"
$fileName = "wbox-virtualstore-probe-$PID.txt"
$realPath = Join-Path $env:ProgramFiles $fileName
$virtualPath = Join-Path $env:LOCALAPPDATA "VirtualStore/Program Files/$fileName"
$containerName = "w3a-$PID"

try {
    New-Item -ItemType Directory -Force -Path $sandbox | Out-Null
    & rustc --target i686-pc-windows-msvc -O $source -o $probeExe
    if ($LASTEXITCODE -ne 0) {
        throw "building the i686 Rust VirtualStore probe failed with rc=$LASTEXITCODE"
    }

    & icacls.exe $sandbox /grant "*S-1-15-2-1:(OI)(CI)(RX)" /T /C | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "granting the AppContainer probe directory RX failed with rc=$LASTEXITCODE"
    }

    Remove-Item -LiteralPath $realPath, $virtualPath -Force -ErrorAction SilentlyContinue
    $output = & $wboxPath run --name $containerName --workdir $env:SystemRoot\System32 -- `
        $probeExe $realPath 2>&1 | Out-String
    $probeExitCode = $LASTEXITCODE
    if ($output -notmatch "POINTER_WIDTH=32 MANIFEST_PRESENT=false") {
        throw "VirtualStore probe preconditions were not met:`n$output"
    }

    [pscustomobject]@{
        ProbeExitCode = $probeExitCode
        RealPath = $realPath
        RealPathExists = Test-Path -LiteralPath $realPath
        VirtualPath = $virtualPath
        VirtualPathExists = Test-Path -LiteralPath $virtualPath
        Output = $output.Trim()
    } | ConvertTo-Json -Depth 3
} finally {
    & $wboxPath rm -f $containerName 2>&1 | Out-Null
    Remove-Item -LiteralPath $realPath, $virtualPath -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $sandbox -Recurse -Force -ErrorAction SilentlyContinue
    $global:LASTEXITCODE = 0
}

param(
    [string]$Exe = (Join-Path $PSScriptRoot '..\target\release\rustcmd.exe')
)

$ErrorActionPreference = 'Stop'
$Exe = [IO.Path]::GetFullPath($Exe)
if (-not (Test-Path -LiteralPath $Exe)) {
    throw "RustCmd executable not found: $Exe"
}

function Invoke-RustCmd {
    param([string[]]$CommandArgs)
    $output = & $Exe @CommandArgs 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "rustcmd $($CommandArgs -join ' ') failed:`n$($output -join "`n")"
    }
    return ($output -join "`n")
}

$hadServer = $true
$savedErrorPreference = $ErrorActionPreference
$ErrorActionPreference = 'SilentlyContinue'
& $Exe list-sessions 2>$null | Out-Null
$probeExitCode = $LASTEXITCODE
$ErrorActionPreference = $savedErrorPreference
if ($probeExitCode -ne 0) {
    $hadServer = $false
}

$name = "rustcmd-smoke-$PID"
$token = "RUSTCMD_SMOKE_$PID"
$targetDir = Join-Path $PSScriptRoot '..\target\smoke'
$windowPng = Join-Path $targetDir "$name-window.png"
$panePng = Join-Path $targetDir "$name-pane.png"
$created = $false

try {
    Write-Host 'STEP create tab'
    Invoke-RustCmd @('new-window', '-d', '-n', $name) | Out-Null
    $created = $true

    Write-Host 'STEP composer round trip'
    Invoke-RustCmd @('set-composer', '-t', $name, "echo $token") | Out-Null
    $draft = Invoke-RustCmd @('show-composer', '-t', $name)
    if ($draft -ne "echo $token") {
        throw "Composer round trip mismatch: [$draft]"
    }

    Write-Host 'STEP submit and wait for output'
    Invoke-RustCmd @('send-composer', '-t', $name) | Out-Null
    Invoke-RustCmd @('wait-pane', '-t', $name, '--contains', $token, '--timeout-ms', '10000') | Out-Null
    $capture = Invoke-RustCmd @('capture-pane', '-p', '-t', $name)
    if (-not $capture.Contains($token)) {
        throw 'capture-pane did not contain the smoke token'
    }

    Write-Host 'STEP screenshots'
    Invoke-RustCmd @('screenshot', '-o', $windowPng) | Out-Null
    Invoke-RustCmd @('screenshot-pane', '-t', $name, '-o', $panePng) | Out-Null
    foreach ($image in @($windowPng, $panePng)) {
        if (-not (Test-Path -LiteralPath $image) -or (Get-Item $image).Length -lt 1000) {
            throw "Screenshot was not created correctly: $image"
        }
    }

    Write-Host 'STEP remain on exit'
    Invoke-RustCmd @('send-keys', '-t', $name, 'exit', 'Enter') | Out-Null
    Invoke-RustCmd @('wait-pane', '-t', $name, '--dead', '--timeout-ms', '10000') | Out-Null
    $state = Invoke-RustCmd @(
        'display-message', '-p', '-t', $name,
        '#{window_id}:#{window_name}:#{pane_dead}'
    )
    if (-not $state.EndsWith(":${name}:1")) {
        throw "Exited tab did not remain visible and dead: $state"
    }

    Write-Host 'STEP explicit close'
    Invoke-RustCmd @('kill-window', '-t', $name) | Out-Null
    $created = $false
    Write-Host "PASS: composer, PTY I/O, waits, capture, PNG screenshots, remain-on-exit, manual close"
}
finally {
    if ($created) {
        & $Exe kill-window -t $name *> $null
    }
    if (-not $hadServer) {
        & $Exe kill-server *> $null
    }
}

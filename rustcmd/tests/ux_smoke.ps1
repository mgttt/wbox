param(
    [string]$Exe = (Join-Path $PSScriptRoot '..\target\release\rustcmd.exe')
)

$ErrorActionPreference = 'Stop'
$Exe = [IO.Path]::GetFullPath($Exe)
if (-not (Test-Path -LiteralPath $Exe)) {
    throw "RustCmd executable not found: $Exe"
}

$previousAddress = $env:RUSTCMD_IPC_ADDRESS
$env:RUSTCMD_IPC_ADDRESS = "127.0.0.1:$((47000 + ($PID % 1000)))"
$name = "ux-smoke-$PID"
$token = "RUSTCMD_UX_$PID"
$draftFile = Join-Path $env:TEMP "$name.txt"

function Invoke-RustCmd {
    param([string[]]$CommandArgs)
    $output = & $Exe @CommandArgs 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "rustcmd $($CommandArgs -join ' ') failed:`n$($output -join "`n")"
    }
    return ($output -join "`n")
}

try {
    Write-Host 'STEP create and discover stable tab'
    Invoke-RustCmd @('new-window', '-d', '-n', $name) | Out-Null
    $snapshot = Invoke-RustCmd @('ui-snapshot') | ConvertFrom-Json
    $tab = $snapshot.tabs | Where-Object name -eq $name
    if ($null -eq $tab -or $tab.state -ne 'running') {
        throw 'ui-snapshot did not expose the running test tab'
    }
    $id = $tab.id

    Write-Host 'STEP two-line tab metadata'
    $note = "build verification $PID"
    Invoke-RustCmd @('set-tab-note', '-t', $id, $note) | Out-Null
    $roundTripNote = Invoke-RustCmd @('show-tab-note', '-t', $id)
    if ($roundTripNote -ne $note) {
        throw "Tab note round trip mismatch: [$roundTripNote]"
    }
    $snapshot = Invoke-RustCmd @('ui-snapshot') | ConvertFrom-Json
    $tab = $snapshot.tabs | Where-Object id -eq $id
    if ($tab.note -ne $note -or [string]::IsNullOrWhiteSpace($tab.terminal_title)) {
        throw 'ui-snapshot did not expose tab note and terminal title'
    }

    Write-Host 'STEP lossless file composer and draft state'
    [IO.File]::WriteAllText($draftFile, "echo $token")
    Invoke-RustCmd @('set-composer', '-t', $id, '--file', $draftFile) | Out-Null
    $snapshot = Invoke-RustCmd @('ui-snapshot') | ConvertFrom-Json
    $tab = $snapshot.tabs | Where-Object id -eq $id
    if (-not $tab.draft) {
        throw 'ui-snapshot did not expose the composer draft'
    }

    Write-Host 'STEP semantic focus and composer send'
    Invoke-RustCmd @('ui-action', 'select-tab', '-t', $id) | Out-Null
    Invoke-RustCmd @('focus', 'composer', '-t', $id) | Out-Null
    Invoke-RustCmd @('wait-ui', '--active', $id, '--focus', 'composer') | Out-Null
    Invoke-RustCmd @('ui-action', 'composer-send', '-t', $id) | Out-Null
    Invoke-RustCmd @('wait-pane', '-t', $id, '--contains', $token, '--timeout-ms', '10000') | Out-Null

    Write-Host 'STEP live close requires confirmation and cancel is safe'
    $snapshot = Invoke-RustCmd @('ui-action', 'close-tab', '-t', $id) | ConvertFrom-Json
    if ($snapshot.modal.kind -ne 'confirm-close-live' -or $snapshot.modal.window_id -ne $id) {
        throw 'live close did not expose the confirmation modal'
    }
    Invoke-RustCmd @('ui-action', 'cancel') | Out-Null
    $snapshot = Invoke-RustCmd @('wait-ui', '-t', $id, '--tab-state', 'running') | ConvertFrom-Json
    if ($null -ne $snapshot.modal) {
        throw 'cancel did not clear the confirmation modal'
    }

    Write-Host 'STEP settings discovery and modal'
    $settings = Invoke-RustCmd @('get-settings') | ConvertFrom-Json
    if ($settings.terminal_font_size -lt 8 -or
        $settings.recommended_cjk_font -ne 'Sarasa Fixed SC') {
        throw 'get-settings did not expose font settings and CJK recommendation'
    }
    $snapshot = Invoke-RustCmd @('ui-action', 'open-settings') | ConvertFrom-Json
    if ($snapshot.modal.kind -ne 'settings' -or
        $snapshot.focus.surface -ne 'settings') {
        throw 'settings modal/focus was not exposed'
    }
    Invoke-RustCmd @('ui-action', 'cancel') | Out-Null

    Write-Host 'STEP dead tab remains and closes without confirmation'
    Invoke-RustCmd @('send-keys', '-t', $id, 'exit', 'Enter') | Out-Null
    Invoke-RustCmd @('wait-ui', '-t', $id, '--tab-state', 'dead', '--timeout-ms', '10000') | Out-Null
    $snapshot = Invoke-RustCmd @('ui-action', 'close-tab', '-t', $id) | ConvertFrom-Json
    if ($snapshot.tabs.id -contains $id -or $null -ne $snapshot.modal) {
        throw 'dead tab was not closed immediately'
    }

    Write-Host 'STEP protocol discovery'
    $protocol = Invoke-RustCmd @('protocol-info') | ConvertFrom-Json
    if ($protocol.protocol_version -ne 1 -or -not $protocol.features.semantic_ui_automation) {
        throw 'protocol-info did not advertise semantic UI automation'
    }
    Write-Host 'PASS: UX state, two-line tabs, settings, composer, focus, safe close, dead close, protocol discovery'
}
finally {
    Remove-Item -LiteralPath $draftFile -ErrorAction SilentlyContinue
    & $Exe kill-server 2>$null | Out-Null
    if ($null -eq $previousAddress) {
        Remove-Item Env:RUSTCMD_IPC_ADDRESS -ErrorAction SilentlyContinue
    } else {
        $env:RUSTCMD_IPC_ADDRESS = $previousAddress
    }
}

param(
    [Parameter(Mandatory = $true)]
    [string]$Wbox
)

$ErrorActionPreference = "Stop"

function Resolve-ExistingFile([string]$Path, [string]$Label) {
    $resolved = Resolve-Path -LiteralPath $Path -ErrorAction Stop
    if (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) {
        throw "$Label is not a file: $resolved"
    }
    return $resolved.Path
}

function Invoke-Wbox(
    [string]$Label,
    [int]$ExpectedExit,
    [string]$Marker,
    [string[]]$Arguments
) {
    $output = & $script:WboxPath @Arguments 2>&1 | Out-String
    $rc = $LASTEXITCODE
    if ($rc -ne $ExpectedExit) {
        throw "$Label failed: expected rc=$ExpectedExit, got rc=$rc`n$output"
    }
    if ($Marker -and $output -notmatch [regex]::Escape($Marker)) {
        throw "$Label did not produce marker '$Marker':`n$output"
    }
    Write-Host "PASS $Label"
    return $output
}

$script:WboxPath = Resolve-ExistingFile $Wbox "wbox"
$system32 = Join-Path $env:SystemRoot "System32"
$powershell = Join-Path $system32 "WindowsPowerShell\v1.0\powershell.exe"
$tag = "{0}-{1}" -f $PID, [Guid]::NewGuid().ToString("N").Substring(0, 8)
$names = @{
    Cmd = "wn-cmd-$tag"
    PowerShell = "wn-ps-$tag"
    Hostname = "wn-host-$tag"
    Whoami = "wn-who-$tag"
    Child = "wn-child-$tag"
    Write = "wn-write-$tag"
    Exit = "wn-exit-$tag"
}
$sandbox = Join-Path ([System.IO.Path]::GetTempPath()) "wbox-native-$tag"
$writable = Join-Path $sandbox "writable"

try {
    New-Item -ItemType Directory -Force -Path $writable | Out-Null
    & icacls.exe $writable /grant "*S-1-15-2-1:(OI)(CI)(M)" /T /C | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "failed to grant AppContainer write access to $writable"
    }

    Invoke-Wbox "WN.1 cmd.exe" 0 "CMD_NATIVE_OK" @(
        "run", "--name", $names.Cmd, "--workdir", $system32, "--",
        "cmd.exe", "/d", "/c", "echo CMD_NATIVE_OK"
    ) | Out-Null

    Invoke-Wbox "WN.2 Windows PowerShell" 0 "POWERSHELL_NATIVE_OK:42" @(
        "run", "--name", $names.PowerShell, "--workdir", $system32, "--",
        $powershell, "-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
        "[Console]::WriteLine('POWERSHELL_NATIVE_OK:' + (6 * 7))"
    ) | Out-Null

    Invoke-Wbox "WN.3 hostname.exe" 0 "" @(
        "run", "--name", $names.Hostname, "--workdir", $system32, "--",
        "hostname.exe"
    ) | Out-Null

    Invoke-Wbox "WN.4 whoami.exe" 0 "\" @(
        "run", "--name", $names.Whoami, "--workdir", $system32, "--",
        "whoami.exe"
    ) | Out-Null

    Invoke-Wbox "WN.5 child process" 0 "CHILD_PROCESS_OK" @(
        "run", "--name", $names.Child, "--workdir", $system32, "--",
        "cmd.exe", "/d", "/c", "cmd.exe /d /c echo CHILD_PROCESS_OK"
    ) | Out-Null

    Invoke-Wbox "WN.6 writable workdir" 0 "" @(
        "run", "--name", $names.Write, "--workdir", $writable, "--",
        "cmd.exe", "/d", "/c", "echo WRITE_OK> sandbox-write.txt"
    ) | Out-Null
    $written = Get-Content -LiteralPath (Join-Path $writable "sandbox-write.txt") -Raw
    if ($written.Trim() -ne "WRITE_OK") {
        throw "WN.6 writable workdir produced unexpected file content: $written"
    }

    Invoke-Wbox "WN.7 exit-code forwarding" 23 "" @(
        "run", "--name", $names.Exit, "--workdir", $system32, "--",
        "cmd.exe", "/d", "/c", "exit /b 23"
    ) | Out-Null

    $ps = & $script:WboxPath ps --all 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) {
        throw "WN.8 state inspection failed:`n$ps"
    }
    foreach ($name in $names.Values) {
        if ($ps -match [regex]::Escape($name)) {
            throw "WN.8 foreground run left state record '$name':`n$ps"
        }
    }
    Write-Host "PASS WN.8 foreground state cleanup"
}
finally {
    $tempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
    $sandboxFull = [System.IO.Path]::GetFullPath($sandbox)
    if ($sandboxFull.StartsWith($tempRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        Remove-Item -LiteralPath $sandboxFull -Recurse -Force -ErrorAction SilentlyContinue
    }
}

param(
    [Parameter(Mandatory = $true)]
    [string]$Wbox,
    [Parameter(Mandatory = $true)]
    [string]$WboxLinux,
    [Parameter(Mandatory = $true)]
    [string]$Busybox
)

$ErrorActionPreference = "Stop"

function Resolve-ExistingFile([string]$Path, [string]$Label) {
    $resolved = Resolve-Path -LiteralPath $Path -ErrorAction Stop
    if (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) {
        throw "$Label is not a file: $resolved"
    }
    return $resolved.Path
}

function Assert-Exit([int]$Expected, [string]$Label, [string]$Details = "") {
    if ($LASTEXITCODE -ne $Expected) {
        throw "$Label failed: expected rc=$Expected, got rc=$LASTEXITCODE`n$Details"
    }
}

function Test-ProcessAlive([int]$ProcessId) {
    try {
        $process = [System.Diagnostics.Process]::GetProcessById($ProcessId)
        return -not $process.HasExited
    } catch [System.ArgumentException] {
        return $false
    }
}

function Wait-PidFile([string]$Path, [int]$TimeoutSeconds) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        if (Test-Path -LiteralPath $Path -PathType Leaf) {
            $text = Get-Content -LiteralPath $Path -Raw -ErrorAction SilentlyContinue
            $value = 0
            if ($null -ne $text -and
                [int]::TryParse($text.Trim(), [ref]$value) -and
                $value -gt 0) {
                return $value
            }
        }
        Start-Sleep -Milliseconds 100
    }
    return $null
}

function Wait-ProcessesExited([int[]]$ProcessIds, [int]$TimeoutSeconds) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        if (-not ($ProcessIds | Where-Object { Test-ProcessAlive $_ })) {
            return $true
        }
        Start-Sleep -Milliseconds 100
    }
    return -not ($ProcessIds | Where-Object { Test-ProcessAlive $_ })
}

$wboxSource = Resolve-ExistingFile $Wbox "wbox"
$linuxSource = Resolve-ExistingFile $WboxLinux "wbox-linux"
$busyboxSource = Resolve-ExistingFile $Busybox "busybox"
$sandbox = Join-Path ([System.IO.Path]::GetTempPath()) (
    "wbox-product-{0}-{1}" -f $PID, [Guid]::NewGuid().ToString("N")
)
$bundle = Join-Path $sandbox "bundle"
$testHome = Join-Path $sandbox "home"
$image = Join-Path $testHome ".wbox\images\local.test\wbox-fixture\latest"
$rootfs = Join-Path $image "rootfs"

$savedUserProfile = $env:USERPROFILE
$savedHome = $env:HOME
$savedWboxLinux = $env:WBOX_LINUX
$savedMarker = $env:HOST_ONLY_MARKER
$portableWbox = $null
$stopName = "product-stop"
$stopPids = @()
$execName = "product-exec"
$crashName = "product-crash"
$writeBgName = "product-write-bg"
$waitName = "product-wait"

try {
    New-Item -ItemType Directory -Force -Path $bundle, $rootfs | Out-Null
    Copy-Item -LiteralPath $wboxSource -Destination (Join-Path $bundle "wbox.exe")
    Copy-Item -LiteralPath $linuxSource -Destination (Join-Path $bundle "wbox-linux.exe")
    Copy-Item -LiteralPath $busyboxSource -Destination (Join-Path $rootfs "busybox")
    New-Item -ItemType Directory -Force -Path (Join-Path $rootfs "probe") | Out-Null
    Set-Content -LiteralPath (Join-Path $rootfs "probe\directory-entry-ok") `
        -Encoding utf8NoBOM -Value "ok"

    Set-Content -LiteralPath (Join-Path $image "manifest.json") -Encoding utf8NoBOM -Value "{}"
    Set-Content -LiteralPath (Join-Path $image "layers.json") -Encoding utf8NoBOM -Value "[]"
    Set-Content -LiteralPath (Join-Path $image "config.json") -Encoding utf8NoBOM -Value @'
{"config":{"Entrypoint":[],"Cmd":["/busybox","echo","PRODUCT_E2E_OK"],"Env":["PATH=/"],"WorkingDir":"/"}}
'@

    # The child runs as an AppContainer token and must be able to read the
    # fixture rootfs. The shipped bundle itself contains exactly two executables.
    & icacls.exe $rootfs /grant "*S-1-15-2-1:(OI)(CI)(RX)" /T /C | Out-Null
    Assert-Exit 0 "rootfs ACL setup"

    $env:USERPROFILE = $testHome
    $env:HOME = $testHome
    Remove-Item Env:WBOX_LINUX -ErrorAction SilentlyContinue

    $portableWbox = Join-Path $bundle "wbox.exe"

    $native = & $portableWbox run --name product-native --workdir $env:SystemRoot\System32 -- `
        cmd.exe /d /c "echo WINDOWS_NATIVE_E2E_OK" 2>&1 | Out-String
    Assert-Exit 0 "WP.1 Windows native product path" $native
    if ($native -notmatch "WINDOWS_NATIVE_E2E_OK") {
        throw "Windows native product path did not produce its marker: $native"
    }
    Write-Host "PASS WP.1 Windows native product path"

    $env:HOST_ONLY_MARKER = "must-not-leak"
    $filtered = & $portableWbox run --name product-env --workdir $env:SystemRoot\System32 -- `
        cmd.exe /d /c "if defined HOST_ONLY_MARKER (exit /b 9) else (echo ENV_FILTER_E2E_OK)" `
        2>&1 | Out-String
    Assert-Exit 0 "WP.2 Windows environment filter product path" $filtered
    if ($filtered -notmatch "ENV_FILTER_E2E_OK") {
        throw "Windows environment filter did not produce its marker: $filtered"
    }
    Write-Host "PASS WP.2 Windows environment filter product path"

    $guest = & $portableWbox run --name product-guest local.test/wbox-fixture:latest 2>&1 | Out-String
    $guestRc = $LASTEXITCODE
    $guestFailure = $null
    if ($guestRc -ne 0) {
        $guestFailure = "WP.3 Windows OCI-to-Linux product path failed: rc=$guestRc`n$guest"
    } elseif ($guest -notmatch "PRODUCT_E2E_OK") {
        $guestFailure = "WP.3 Windows OCI-to-Linux product path did not produce its marker: $guest"
    } else {
        Write-Host "PASS WP.3 Windows OCI-to-Linux product path"
        Write-Host "PASS WP.4 portable two-executable bundle"
    }

    # AppContainer can deny GetFinalPathNameByHandleW while still allowing
    # directory reads. wbox-linux must enumerate through the path remembered
    # by openat(), otherwise language runtimes such as Python see empty stdlib
    # directories and fail to import even `encodings`.
    $directoryGuest = & $portableWbox run --name product-dir local.test/wbox-fixture:latest `
        /busybox ls /probe 2>&1 | Out-String
    Assert-Exit 0 "WP.3D AppContainer Linux directory enumeration" $directoryGuest
    if ($directoryGuest -notmatch "directory-entry-ok") {
        throw "WP.3D directory enumeration did not return the fixture entry: $directoryGuest"
    }
    Write-Host "PASS WP.3D AppContainer Linux directory enumeration"

    $writeGuest = & $portableWbox run --name product-write local.test/wbox-fixture:latest `
        /busybox sh -c 'echo PRIVATE_WRITE_OK > /probe/private-write && /busybox cat /probe/private-write' `
        2>&1 | Out-String
    Assert-Exit 0 "WP.3W Windows OCI private writable rootfs" $writeGuest
    if ($writeGuest -notmatch "PRIVATE_WRITE_OK") {
        throw "WP.3W private writable rootfs did not produce its marker: $writeGuest"
    }
    if (Test-Path -LiteralPath (Join-Path $rootfs "probe\private-write")) {
        throw "WP.3W guest write modified the shared image cache"
    }
    if (Test-Path -LiteralPath (Join-Path $testHome ".wbox\run\product-write")) {
        throw "WP.3W foreground private rootfs was not cleaned after exit"
    }
    Write-Host "PASS WP.3W Windows OCI private writable rootfs and cache isolation"

    $writeBg = & $portableWbox run -d --name $writeBgName local.test/wbox-fixture:latest `
        /busybox sh -c 'echo PRIVATE_BG_OK > /probe/private-bg && /busybox cat /probe/private-bg' `
        2>&1 | Out-String
    Assert-Exit 0 "WP.3WB detached private rootfs launch" $writeBg
    $writeBgDir = Join-Path $testHome ".wbox\run\$writeBgName"
    $writeBgFile = Join-Path $writeBgDir "rootfs\probe\private-bg"
    foreach ($i in 1..40) {
        if (Test-Path -LiteralPath $writeBgFile -PathType Leaf) { break }
        Start-Sleep -Milliseconds 250
    }
    if (-not (Test-Path -LiteralPath $writeBgFile -PathType Leaf)) {
        throw "WP.3WB detached private rootfs did not persist its guest file"
    }
    if (Test-Path -LiteralPath (Join-Path $rootfs "probe\private-bg")) {
        throw "WP.3WB detached guest modified the shared image cache"
    }
    $writeBgRemoved = $false
    $writeBgRemoveOutput = ""
    foreach ($i in 1..40) {
        $writeBgRemoveOutput = & $portableWbox rm $writeBgName 2>&1 | Out-String
        if ($LASTEXITCODE -eq 0) {
            $writeBgRemoved = $true
            break
        }
        Start-Sleep -Milliseconds 250
    }
    if (-not $writeBgRemoved) {
        throw "WP.3WB could not remove exited private rootfs: $writeBgRemoveOutput"
    }
    if (Test-Path -LiteralPath $writeBgDir) {
        throw "WP.3WB wbox rm did not remove the private rootfs"
    }
    Write-Host "PASS WP.3WB detached private rootfs persists until rm"

    $runtimeHelp = & (Join-Path $bundle "wbox-linux.exe") --help 2>&1 | Out-String
    Assert-Exit 0 "WP.4 wbox-linux identity help" $runtimeHelp
    if ($runtimeHelp -notmatch "internal Linux ELF runtime" -or
        $runtimeHelp -notmatch "Use wbox.exe") {
        throw "WP.4 wbox-linux help did not identify the container CLI: $runtimeHelp"
    }

    $traceInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $traceInfo.FileName = Join-Path $bundle "wbox-linux.exe"
    $traceInfo.WorkingDirectory = $rootfs
    $traceInfo.UseShellExecute = $false
    $traceInfo.RedirectStandardOutput = $true
    $traceInfo.RedirectStandardError = $true
    $traceInfo.Environment["BLINK_PREFIX"] = $rootfs
    @("-s", "-e", "/busybox", "true") | ForEach-Object {
        [void]$traceInfo.ArgumentList.Add($_)
    }
    $traceProcess = [System.Diagnostics.Process]::Start($traceInfo)
    $traceStdout = $traceProcess.StandardOutput.ReadToEndAsync()
    $traceStderr = $traceProcess.StandardError.ReadToEndAsync()
    if (-not $traceProcess.WaitForExit(10000)) {
        $traceProcess.Kill($true)
        throw "WP.4S wbox-linux syscall trace timed out"
    }
    $trace = $traceStdout.Result + $traceStderr.Result
    if ($traceProcess.ExitCode -ne 0 -or $trace -notmatch "\(sys\)") {
        throw "WP.4S release syscall trace produced no syscall records: $trace"
    }
    Write-Host "PASS WP.4S release syscall trace"

    $ps = & $portableWbox ps --all 2>&1 | Out-String
    Assert-Exit 0 "post-run state inspection" $ps
    if ($ps -match "product-(native|env|guest)") {
        throw "normal foreground runs left a state record: $ps"
    }
    Write-Host "PASS WP.5 normal-run state cleanup"

    # WP.6 backgrounded lifecycle on Windows: --detach -> ps -> logs -> rm.
    # Linux gates this end to end (P.9-P.18); Windows previously covered only
    # the two platform-specific pieces by unit test (lock semantics, process
    # termination). Without this the README's lifecycle claim is unverified
    # on the very host the product targets.
    $bg = & $portableWbox run -d --name product-bg --workdir $env:SystemRoot\System32 -- `
        cmd.exe /d /c "echo DETACH_E2E_OK" 2>&1 | Out-String
    Assert-Exit 0 "WP.6 detach launch" $bg
    if ($bg.Trim() -ne "product-bg") {
        throw "WP.6 detach should print just the container name, got: $bg"
    }

    # The container is short-lived; wait for it to be recorded as exited.
    # Detached records persist after exit by design (that is what makes
    # `logs` useful afterwards), so this must not race on cleanup.
    $seen = $false
    foreach ($i in 1..30) {
        Start-Sleep -Milliseconds 500
        $psAll = & $portableWbox ps --all 2>&1 | Out-String
        if ($psAll -match "product-bg") { $seen = $true; break }
    }
    if (-not $seen) {
        throw "WP.6 detached container never appeared in ps --all"
    }

    $bgLog = & $portableWbox logs product-bg 2>&1 | Out-String
    Assert-Exit 0 "WP.6 logs read" $bgLog
    if ($bgLog -notmatch "DETACH_E2E_OK") {
        throw "WP.6 logs did not contain the guest marker: $bgLog"
    }
    Write-Host "PASS WP.6 Windows detach -> ps -> logs"

    & $portableWbox rm product-bg 2>&1 | Out-Null
    Assert-Exit 0 "WP.7 rm detached record"
    $psAfter = & $portableWbox ps --all 2>&1 | Out-String
    if ($psAfter -match "product-bg") {
        throw "WP.7 rm left the record behind: $psAfter"
    }
    Write-Host "PASS WP.7 rm clears the detached record"

    $waitLaunch = & $portableWbox run -d --name $waitName local.test/wbox-fixture:latest `
        /busybox sh -c 'exit 37' 2>&1 | Out-String
    Assert-Exit 0 "WP.7B detached wait workload launch" $waitLaunch
    $waitOutput = & $portableWbox wait $waitName 2>&1 | Out-String
    Assert-Exit 0 "WP.7B wait command" $waitOutput
    if ($waitOutput.Trim() -ne "37") {
        throw "WP.7B wait did not print the guest exit code: $waitOutput"
    }
    $containerInspectText = & $portableWbox container inspect $waitName 2>&1 | Out-String
    Assert-Exit 0 "WP.7C container inspect alias" $containerInspectText
    $containerInspect = @($containerInspectText | ConvertFrom-Json)[0]
    if ($containerInspect.State.Status -ne "exited" -or
        [int]$containerInspect.State.ExitCode -ne 37 -or
        $containerInspect.Config.Image -ne "local.test/wbox-fixture:latest") {
        throw "WP.7C container inspect returned the wrong state: $containerInspectText"
    }
    $imageInspectText = & $portableWbox image inspect local.test/wbox-fixture:latest `
        2>&1 | Out-String
    Assert-Exit 0 "WP.7C image inspect" $imageInspectText
    $imageInspect = @($imageInspectText | ConvertFrom-Json)[0]
    if ($imageInspect.Os -ne "linux" -or
        $imageInspect.Config.Cmd[0] -ne "/busybox") {
        throw "WP.7C image inspect returned the wrong config: $imageInspectText"
    }
    & $portableWbox rm $waitName 2>&1 | Out-Null
    Assert-Exit 0 "WP.7C rm waited record"
    Write-Host "PASS WP.7B wait persists and reports the guest exit code"
    Write-Host "PASS WP.7C container/image inspect emits machine-readable JSON"

    $autoRemoveName = "product-auto-rm"
    $autoRemove = & $portableWbox run -d --rm --name $autoRemoveName `
        --workdir $env:SystemRoot\System32 -- cmd.exe /d /c "exit /b 0" 2>&1 | Out-String
    Assert-Exit 0 "WP.7A detached --rm launch" $autoRemove
    $autoRemoveDir = Join-Path $testHome ".wbox\run\$autoRemoveName"
    foreach ($i in 1..30) {
        if (-not (Test-Path -LiteralPath $autoRemoveDir)) { break }
        Start-Sleep -Milliseconds 200
    }
    if (Test-Path -LiteralPath $autoRemoveDir) {
        throw "WP.7A --rm left a detached state directory: $autoRemoveDir"
    }
    Write-Host "PASS WP.7A detached --rm removes state and logs"

    # WP.8-WP.12 continuously gate the Windows stop contract against a real
    # supervisor -> guest -> child tree. Every process is identified by a PID
    # written under this run's unique temp directory; never scan or kill by
    # executable name because the host may have unrelated PowerShell processes.
    $stopWork = Join-Path $sandbox "stop-work"
    New-Item -ItemType Directory -Force -Path $stopWork | Out-Null
    & icacls.exe $stopWork /grant "*S-1-15-2-1:(OI)(CI)(M)" /T /C | Out-Null
    Assert-Exit 0 "WP.8 stop workload ACL setup"

    $guestPidFile = Join-Path $stopWork "guest.pid"
    $childPidFile = Join-Path $stopWork "child.pid"
    $workloadScript = Join-Path $stopWork "workload.ps1"
    Set-Content -LiteralPath $workloadScript -Encoding utf8NoBOM -Value @'
param(
    [Parameter(Mandatory = $true)]
    [string]$GuestPidFile,
    [Parameter(Mandatory = $true)]
    [string]$ChildPidFile
)

[System.IO.File]::WriteAllText($GuestPidFile, [string]$PID)
$startInfo = [System.Diagnostics.ProcessStartInfo]::new()
$startInfo.FileName = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
$startInfo.Arguments = '-NoLogo -NoProfile -NonInteractive -Command "[Threading.Thread]::Sleep(3600000)"'
$startInfo.UseShellExecute = $false
$startInfo.CreateNoWindow = $true
$child = [System.Diagnostics.Process]::Start($startInfo)
[System.IO.File]::WriteAllText($ChildPidFile, [string]$child.Id)
while ($true) {
    [System.Threading.Thread]::Sleep(60000)
}
'@

    # Do not capture stdout here. A long-lived detached supervisor can inherit
    # PowerShell's native-command pipe handle, making `... | Out-String` wait
    # for EOF until the container exits. Wait on the short-lived parent wbox
    # process handle instead; its exit code is the contract under test.
    $launchInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $launchInfo.FileName = $portableWbox
    $launchInfo.UseShellExecute = $false
    $launchInfo.CreateNoWindow = $true
    $launchInfo.Arguments = @(
        "run", "-d", "--name", $stopName, "--workdir", $stopWork, "--",
        "powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive",
        "-File", $workloadScript,
        "-GuestPidFile", $guestPidFile,
        "-ChildPidFile", $childPidFile
    ) -join " "
    $launcher = [System.Diagnostics.Process]::Start($launchInfo)
    if (-not $launcher.WaitForExit(15000)) {
        $launcher.Kill()
        throw "WP.8 detached launch parent did not exit within 15 seconds"
    }
    if ($launcher.ExitCode -ne 0) {
        throw "WP.8 detached stop workload launch failed: rc=$($launcher.ExitCode)"
    }

    $metaFile = Join-Path $testHome ".wbox\run\$stopName\meta.json"
    $guestPid = Wait-PidFile $guestPidFile 15
    $childPid = Wait-PidFile $childPidFile 15
    if (-not (Test-Path -LiteralPath $metaFile -PathType Leaf) -or
        $null -eq $guestPid -or
        $null -eq $childPid) {
        throw "WP.8 timed out waiting for non-empty, parseable supervisor/guest/child PID markers"
    }
    $supervisorPid = [int]((Get-Content -LiteralPath $metaFile -Raw | ConvertFrom-Json).pid)
    $stopPids = @($supervisorPid, $guestPid, $childPid)
    if (($stopPids | Select-Object -Unique).Count -ne 3) {
        throw "WP.8 expected three distinct process IDs, got: $($stopPids -join ', ')"
    }
    $missingBeforeStop = $stopPids | Where-Object { -not (Test-ProcessAlive $_) }
    if ($missingBeforeStop) {
        throw "WP.8 process tree was not alive before stop; missing PID(s): $($missingBeforeStop -join ', ')"
    }
    Write-Host "PASS WP.8 detached Windows supervisor -> guest -> child tree"

    $stopped = & $portableWbox stop $stopName 2>&1 | Out-String
    Assert-Exit 0 "WP.9 stop running Windows workload" $stopped
    if (-not (Wait-ProcessesExited $stopPids 15)) {
        $survivors = $stopPids | Where-Object { Test-ProcessAlive $_ }
        throw "WP.9 stop left process PID(s) alive: $($survivors -join ', ')"
    }
    $stoppedState = & $portableWbox ps --all 2>&1 | Out-String
    Assert-Exit 0 "WP.9 stopped state inspection" $stoppedState
    if ($stoppedState -notmatch "(?m)^$([regex]::Escape($stopName))\s+exited\s+") {
        throw "WP.9 stopped container was not recorded as exited: $stoppedState"
    }
    Write-Host "PASS WP.9 stop exits container and removes its complete process tree"

    $stoppedAgain = & $portableWbox stop $stopName 2>&1 | Out-String
    Assert-Exit 0 "WP.10 idempotent stop" $stoppedAgain
    Write-Host "PASS WP.10 stop is idempotent"

    $missingName = "$stopName-missing-$PID"
    $missingStop = & $portableWbox stop $missingName 2>&1 | Out-String
    $missingStopRc = $LASTEXITCODE
    if ($missingStopRc -eq 0) {
        throw "WP.11 stop of unknown container unexpectedly succeeded: $missingStop"
    }
    Write-Host "PASS WP.11 stop rejects an unknown container name"

    & $portableWbox rm $stopName 2>&1 | Out-Null
    Assert-Exit 0 "WP.12 rm stopped record"
    $stopPids = @()
    $stopAfterRm = & $portableWbox ps --all 2>&1 | Out-String
    Assert-Exit 0 "WP.12 post-rm state inspection" $stopAfterRm
    if ($stopAfterRm -match "(?m)^$([regex]::Escape($stopName))\s+") {
        throw "WP.12 rm left the stopped record behind: $stopAfterRm"
    }
    Write-Host "PASS WP.12 rm cleans the stopped record"

    # WP.13-WP.16 gate the Windows-native exec subset. It deliberately accepts
    # the Docker/Podman positional form without a mandatory `--`.
    $execWork = Join-Path $sandbox "exec-work"
    New-Item -ItemType Directory -Force -Path $execWork | Out-Null
    & icacls.exe $execWork /grant "*S-1-15-2-1:(OI)(CI)(M)" /T /C | Out-Null
    Assert-Exit 0 "WP.13 exec workload ACL setup"

    $execLaunchInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $execLaunchInfo.FileName = $portableWbox
    $execLaunchInfo.UseShellExecute = $false
    $execLaunchInfo.CreateNoWindow = $true
    @(
        "run", "-d", "--name", $execName, "--workdir", $execWork, "--",
        "powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive",
        "-Command", "[Threading.Thread]::Sleep(120000)"
    ) | ForEach-Object { [void]$execLaunchInfo.ArgumentList.Add($_) }
    $execLauncher = [System.Diagnostics.Process]::Start($execLaunchInfo)
    if (-not $execLauncher.WaitForExit(15000)) {
        $execLauncher.Kill()
        throw "WP.13 detached exec workload parent did not exit within 15 seconds"
    }
    if ($execLauncher.ExitCode -ne 0) {
        throw "WP.13 detached exec workload launch failed: rc=$($execLauncher.ExitCode)"
    }

    $execSeen = $false
    foreach ($i in 1..50) {
        $execPs = & $portableWbox ps 2>&1 | Out-String
        if ($execPs -match "(?m)^$([regex]::Escape($execName))\s+running\s+") {
            $execSeen = $true
            break
        }
        Start-Sleep -Milliseconds 200
    }
    if (-not $execSeen) {
        throw "WP.13 detached native container never became running: $execPs"
    }

    $execMarker = Join-Path $execWork "exec-result.txt"
    & $portableWbox exec $execName powershell.exe -NoLogo -NoProfile -NonInteractive `
        -Command "[IO.File]::WriteAllText('exec-result.txt','EXEC_WINDOWS_OK')"
    Assert-Exit 0 "WP.13 Windows native exec marker"
    if (-not (Test-Path -LiteralPath $execMarker -PathType Leaf) -or
        (Get-Content -LiteralPath $execMarker -Raw).Trim() -ne "EXEC_WINDOWS_OK") {
        throw "WP.13 exec did not write its marker in the inherited workdir"
    }
    Write-Host "PASS WP.13 Windows native exec accepts Docker/Podman positional syntax"

    & $portableWbox exec $execName -- cmd.exe /d /c "exit 23"
    if ($LASTEXITCODE -ne 23) {
        throw "WP.14 exec exit forwarding failed: expected rc=23, got rc=$LASTEXITCODE"
    }
    Write-Host "PASS WP.14 Windows native exec forwards the guest exit code"

    $longExecInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $longExecInfo.FileName = $portableWbox
    $longExecInfo.UseShellExecute = $false
    $longExecInfo.CreateNoWindow = $true
    @(
        "exec", $execName, "--",
        "powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive",
        "-Command", "[Threading.Thread]::Sleep(120000)"
    ) | ForEach-Object { [void]$longExecInfo.ArgumentList.Add($_) }
    $longExec = [System.Diagnostics.Process]::Start($longExecInfo)
    Start-Sleep -Milliseconds 800
    if ($longExec.HasExited) {
        throw "WP.15 concurrent exec exited before stop: rc=$($longExec.ExitCode)"
    }

    & $portableWbox stop $execName --timeout 5 2>&1 | Out-Null
    Assert-Exit 0 "WP.15 stop with concurrent exec"
    if (-not $longExec.WaitForExit(10000)) {
        $longExec.Kill()
        throw "WP.15 stop did not release the concurrent exec controller"
    }
    if ($longExec.ExitCode -eq 0) {
        throw "WP.15 interrupted exec unexpectedly returned rc=0"
    }
    Write-Host "PASS WP.15 stop terminates the shared Job and concurrent exec"

    $deadExec = & $portableWbox exec $execName -- cmd.exe /d /c "echo SHOULD_NOT_RUN" 2>&1 | Out-String
    $deadExecRc = $LASTEXITCODE
    if ($deadExecRc -eq 0 -or $deadExec -notmatch "已退出") {
        throw "WP.16 exec on an exited container was not rejected clearly: rc=$deadExecRc`n$deadExec"
    }
    & $portableWbox rm $execName 2>&1 | Out-Null
    Assert-Exit 0 "WP.16 rm exec record"
    Write-Host "PASS WP.16 exec rejects an exited container"

    # An exec controller must close its Job handle immediately after attaching.
    # Otherwise killing the supervisor would no longer trigger KILL_ON_JOB_CLOSE.
    $crashGuestPidFile = Join-Path $execWork "crash-guest.pid"
    $crashExecPidFile = Join-Path $execWork "crash-exec.pid"
    $crashLaunchInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $crashLaunchInfo.FileName = $portableWbox
    $crashLaunchInfo.UseShellExecute = $false
    $crashLaunchInfo.CreateNoWindow = $true
    @(
        "run", "-d", "--name", $crashName, "--workdir", $execWork, "--",
        "powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
        "[IO.File]::WriteAllText('crash-guest.pid',[string]`$PID);[Threading.Thread]::Sleep(120000)"
    ) | ForEach-Object { [void]$crashLaunchInfo.ArgumentList.Add($_) }
    $crashLauncher = [System.Diagnostics.Process]::Start($crashLaunchInfo)
    if (-not $crashLauncher.WaitForExit(15000)) {
        $crashLauncher.Kill()
        throw "WP.17 detached crash workload parent did not exit within 15 seconds"
    }
    if ($crashLauncher.ExitCode -ne 0) {
        throw "WP.17 detached crash workload launch failed: rc=$($crashLauncher.ExitCode)"
    }
    $crashGuestPid = Wait-PidFile $crashGuestPidFile 15
    if ($null -eq $crashGuestPid) {
        throw "WP.17 main guest did not publish its PID"
    }

    $crashExecInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $crashExecInfo.FileName = $portableWbox
    $crashExecInfo.UseShellExecute = $false
    $crashExecInfo.CreateNoWindow = $true
    @(
        "exec", $crashName, "powershell.exe", "-NoLogo", "-NoProfile",
        "-NonInteractive", "-Command",
        "[IO.File]::WriteAllText('crash-exec.pid',[string]`$PID);[Threading.Thread]::Sleep(120000)"
    ) | ForEach-Object { [void]$crashExecInfo.ArgumentList.Add($_) }
    $crashExecController = [System.Diagnostics.Process]::Start($crashExecInfo)
    $crashExecPid = Wait-PidFile $crashExecPidFile 15
    $crashMetaFile = Join-Path $testHome ".wbox\run\$crashName\meta.json"
    if ($null -eq $crashExecPid -or -not (Test-Path -LiteralPath $crashMetaFile -PathType Leaf)) {
        throw "WP.17 exec guest or supervisor metadata did not appear"
    }
    $crashSupervisorPid = [int]((Get-Content -LiteralPath $crashMetaFile -Raw | ConvertFrom-Json).pid)
    Stop-Process -Id $crashSupervisorPid -Force
    if (-not (Wait-ProcessesExited @($crashSupervisorPid, $crashGuestPid, $crashExecPid) 15)) {
        throw "WP.17 supervisor crash left a guest alive"
    }
    if (-not $crashExecController.WaitForExit(10000)) {
        $crashExecController.Kill()
        throw "WP.17 exec controller did not observe its guest termination"
    }
    & $portableWbox rm $crashName 2>&1 | Out-Null
    Assert-Exit 0 "WP.17 rm crashed record"
    Write-Host "PASS WP.17 supervisor crash closes the Job and all attached guests"

    if ($null -ne $guestFailure) {
        throw $guestFailure
    }
}
finally {
    if ($null -ne $portableWbox -and (Test-Path -LiteralPath $portableWbox -PathType Leaf)) {
        & $portableWbox stop $stopName 2>&1 | Out-Null
        & $portableWbox rm $stopName 2>&1 | Out-Null
        & $portableWbox stop $execName 2>&1 | Out-Null
        & $portableWbox rm $execName 2>&1 | Out-Null
        & $portableWbox stop $crashName 2>&1 | Out-Null
        & $portableWbox rm $crashName 2>&1 | Out-Null
        & $portableWbox stop $writeBgName 2>&1 | Out-Null
        & $portableWbox rm $writeBgName 2>&1 | Out-Null
        & $portableWbox stop $waitName 2>&1 | Out-Null
        & $portableWbox rm $waitName 2>&1 | Out-Null
    }
    foreach ($processId in $stopPids) {
        if (Test-ProcessAlive $processId) {
            Stop-Process -Id $processId -Force -ErrorAction SilentlyContinue
        }
    }
    if ($null -eq $savedUserProfile) {
        Remove-Item Env:USERPROFILE -ErrorAction SilentlyContinue
    } else {
        $env:USERPROFILE = $savedUserProfile
    }
    if ($null -eq $savedHome) {
        Remove-Item Env:HOME -ErrorAction SilentlyContinue
    } else {
        $env:HOME = $savedHome
    }
    if ($null -eq $savedWboxLinux) {
        Remove-Item Env:WBOX_LINUX -ErrorAction SilentlyContinue
    } else {
        $env:WBOX_LINUX = $savedWboxLinux
    }
    if ($null -eq $savedMarker) {
        Remove-Item Env:HOST_ONLY_MARKER -ErrorAction SilentlyContinue
    } else {
        $env:HOST_ONLY_MARKER = $savedMarker
    }
    Remove-Item -LiteralPath $sandbox -Recurse -Force -ErrorAction SilentlyContinue
    # Cleanup intentionally probes names that may already have been removed.
    # Do not let the last ignored native rc become the script's process rc.
    $global:LASTEXITCODE = 0
}

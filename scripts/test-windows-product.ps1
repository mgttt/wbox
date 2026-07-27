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

try {
    New-Item -ItemType Directory -Force -Path $bundle, $rootfs | Out-Null
    Copy-Item -LiteralPath $wboxSource -Destination (Join-Path $bundle "wbox.exe")
    Copy-Item -LiteralPath $linuxSource -Destination (Join-Path $bundle "wbox-linux.exe")
    Copy-Item -LiteralPath $busyboxSource -Destination (Join-Path $rootfs "busybox")

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

    if ($null -ne $guestFailure) {
        throw $guestFailure
    }
}
finally {
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
}

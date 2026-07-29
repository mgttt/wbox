param(
    [Parameter(Mandatory = $true)]
    [string]$Wbox,
    [Parameter(Mandatory = $true)]
    [string]$WboxLinux,
    [Parameter(Mandatory = $true)]
    [string]$UbuntuImage
)

$ErrorActionPreference = "Stop"
if ($null -ne (Get-Variable PSStyle -ErrorAction SilentlyContinue)) {
    $PSStyle.OutputRendering = "PlainText"
}

$expectedManifest = "52df9b1ee71626e0088f7d400d5c6b5f7bb916f8f0c82b474289a4ece6cf3faf"

function Resolve-ExistingFile([string]$Path, [string]$Label) {
    $resolved = Resolve-Path -LiteralPath $Path -ErrorAction Stop
    if (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) {
        throw "$Label is not a file: $resolved"
    }
    return $resolved.Path
}

function Resolve-ExistingDirectory([string]$Path, [string]$Label) {
    $resolved = Resolve-Path -LiteralPath $Path -ErrorAction Stop
    if (-not (Test-Path -LiteralPath $resolved -PathType Container)) {
        throw "$Label is not a directory: $resolved"
    }
    return $resolved.Path
}

$wboxSource = Resolve-ExistingFile $Wbox "wbox"
$linuxSource = Resolve-ExistingFile $WboxLinux "wbox-linux"
$ubuntuSource = Resolve-ExistingDirectory $UbuntuImage "Ubuntu image fixture"
foreach ($required in @("rootfs", "manifest.json", "config.json", "layers.json")) {
    if (-not (Test-Path -LiteralPath (Join-Path $ubuntuSource $required))) {
        throw "Ubuntu image fixture is missing $required"
    }
}

$manifest = Join-Path $ubuntuSource "manifest.json"
$fixtureMetadata = Join-Path $ubuntuSource "fixture.json"
$manifestHash = (Get-FileHash -LiteralPath $manifest -Algorithm SHA256).Hash.ToLowerInvariant()
$provenanceOk = $manifestHash -eq $expectedManifest
if (Test-Path -LiteralPath $fixtureMetadata -PathType Leaf) {
    $fixture = Get-Content -LiteralPath $fixtureMetadata -Raw | ConvertFrom-Json
    $provenanceOk = $provenanceOk -or (
        $fixture.source -eq "ubuntu@sha256:$expectedManifest" -and
        $fixture.os -eq "linux" -and
        $fixture.architecture -eq "amd64"
    )
}
if (-not $provenanceOk) {
    throw "Ubuntu fixture does not prove pinned linux/amd64 manifest sha256:$expectedManifest"
}

$sandbox = Join-Path ([System.IO.Path]::GetTempPath()) (
    "wbox-ubuntu-{0}-{1}" -f $PID, [Guid]::NewGuid().ToString("N")
)
$bundle = Join-Path $sandbox "bundle"
$testHome = Join-Path $sandbox "home"
$image = Join-Path $testHome ".wbox\images\local.test\ubuntu-24.04\latest"
$savedUserProfile = $env:USERPROFILE
$savedHome = $env:HOME
$savedWboxLinux = $env:WBOX_LINUX

try {
    New-Item -ItemType Directory -Force -Path $bundle, $image | Out-Null
    Copy-Item -LiteralPath $wboxSource -Destination (Join-Path $bundle "wbox.exe")
    Copy-Item -LiteralPath $linuxSource -Destination (Join-Path $bundle "wbox-linux.exe")
    Copy-Item -Path (Join-Path $ubuntuSource "*") -Destination $image -Recurse -Force
    & icacls.exe (Join-Path $image "rootfs") /grant "*S-1-15-2-1:(OI)(CI)(RX)" /T /C |
        Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "WU.1 Ubuntu rootfs ACL setup failed: rc=$LASTEXITCODE"
    }
    Write-Host "PASS WU.1 pinned Ubuntu 24.04 amd64 fixture and AppContainer ACL"

    $env:USERPROFILE = $testHome
    $env:HOME = $testHome
    Remove-Item Env:WBOX_LINUX -ErrorAction SilentlyContinue
    $portableWbox = Join-Path $bundle "wbox.exe"

    $probe = @'
set -eu
. /etc/os-release
printf 'OS=%s:%s\n' "$ID" "$VERSION_ID"
/usr/bin/uname -m
/bin/bash --version
/usr/bin/apt --version
/usr/bin/dpkg --print-architecture
/usr/bin/getconf LONG_BIT
exit 37
'@
    $output = & $portableWbox run --name product-ubuntu `
        local.test/ubuntu-24.04:latest /bin/bash -c $probe 2>&1 | Out-String
    $runRc = $LASTEXITCODE
    if ($runRc -ne 37) {
        throw "WU.2 Ubuntu probe failed: expected rc=37, got rc=$runRc`n$output"
    }
    foreach ($required in @(
            "OS=ubuntu:24.04",
            "x86_64",
            "GNU bash, version 5.2",
            "apt 2.8.3 (amd64)",
            "amd64",
            "64"
        )) {
        if ($output -notmatch [regex]::Escape($required)) {
            throw "WU.2 Ubuntu probe lost '$required': $output"
        }
    }
    $state = Join-Path $testHome ".wbox\run\product-ubuntu"
    if (Test-Path -LiteralPath $state) {
        throw "WU.2 foreground Ubuntu probe left container state: $state"
    }
    Write-Host "PASS WU.2 Ubuntu 24.04 glibc/Bash/APT/dpkg/getconf product path and rc=37"
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
    Remove-Item -LiteralPath $sandbox -Recurse -Force -ErrorAction SilentlyContinue
    $global:LASTEXITCODE = 0
}

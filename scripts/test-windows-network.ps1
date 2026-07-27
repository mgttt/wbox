param(
    [Parameter(Mandatory = $true)]
    [string]$Wbox,
    [string]$Endpoint = "http://1.1.1.1/cdn-cgi/trace",
    [ValidateRange(5, 120)]
    [int]$TimeoutSeconds = 25
)

$ErrorActionPreference = "Stop"

function Resolve-ExistingFile([string]$Path, [string]$Label) {
    $resolved = Resolve-Path -LiteralPath $Path -ErrorAction Stop
    if (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) {
        throw "$Label is not a file: $resolved"
    }
    return $resolved.Path
}

function Invoke-BoundedProcess(
    [string]$FilePath,
    [string[]]$Arguments,
    [int]$Timeout
) {
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $FilePath
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    foreach ($argument in $Arguments) {
        $startInfo.ArgumentList.Add($argument)
    }

    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    try {
        if (-not $process.Start()) {
            throw "failed to start: $FilePath"
        }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($Timeout * 1000)) {
            $process.Kill($true)
            $process.WaitForExit()
            throw "timed out after ${Timeout}s: $FilePath $($Arguments -join ' ')"
        }
        $process.WaitForExit()
        return [pscustomobject]@{
            ExitCode = $process.ExitCode
            Output = $stdout.GetAwaiter().GetResult() + $stderr.GetAwaiter().GetResult()
        }
    }
    finally {
        $process.Dispose()
    }
}

function Assert-ReachedEndpoint([string]$Label, $Result) {
    if ($Result.ExitCode -ne 0) {
        throw "$Label failed: rc=$($Result.ExitCode)`n$($Result.Output)"
    }
    Write-Host "PASS $Label"
}

function Assert-NoRunState([string[]]$Names) {
    $result = Invoke-BoundedProcess $script:WboxPath @("ps", "--all") 10
    if ($result.ExitCode -ne 0) {
        throw "state inspection failed: rc=$($result.ExitCode)`n$($result.Output)"
    }
    foreach ($name in $Names) {
        if ($result.Output -match [regex]::Escape($name)) {
            throw "foreground network run left state record '$name':`n$($result.Output)"
        }
    }
    Write-Host "PASS WNET.4 foreground state cleanup"
}

$script:WboxPath = Resolve-ExistingFile $Wbox "wbox"
$curl = Resolve-ExistingFile (Join-Path $env:SystemRoot "System32\curl.exe") "curl"
$system32 = Join-Path $env:SystemRoot "System32"
$tag = "{0}-{1}" -f $PID, [Guid]::NewGuid().ToString("N").Substring(0, 10)
$defaultName = "wnet-deny-$tag"
$allowedName = "wnet-allow-$tag"
$runNames = @($defaultName, $allowedName)
$stateChecked = $false
$sandbox = Join-Path ([System.IO.Path]::GetTempPath()) "wbox-network-$tag"
$curlCommonArguments = @(
    "--silent",
    "--show-error",
    "--fail",
    "--connect-timeout", "8",
    "--max-time", "15"
)

try {
    New-Item -ItemType Directory -Force -Path $sandbox | Out-Null
    & icacls.exe $sandbox /grant "*S-1-15-2-1:(OI)(CI)(M)" /T /C | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "failed to grant AppContainer write access to $sandbox"
    }

    $hostCurlArguments = $curlCommonArguments + @(
        "--output", (Join-Path $sandbox "host-response.txt"), $Endpoint
    )
    $hostResult = Invoke-BoundedProcess $curl $hostCurlArguments $TimeoutSeconds
    Assert-ReachedEndpoint "WNET.1 host endpoint reachable" $hostResult

    $defaultCurlArguments = $curlCommonArguments + @(
        "--output", (Join-Path $sandbox "default-response.txt"), $Endpoint
    )
    $defaultArguments = @(
        "run", "--name", $defaultName, "--workdir", $system32, "--",
        $curl
    ) + $defaultCurlArguments
    $defaultResult = Invoke-BoundedProcess $script:WboxPath $defaultArguments $TimeoutSeconds
    if ($defaultResult.ExitCode -ne 7) {
        throw "WNET.2 expected curl connection failure rc=7, got rc=$($defaultResult.ExitCode)`n$($defaultResult.Output)"
    }
    Write-Host "PASS WNET.2 default AppContainer denied"

    $allowedCurlArguments = $curlCommonArguments + @(
        "--output", (Join-Path $sandbox "allowed-response.txt"), $Endpoint
    )
    $allowedArguments = @(
        "run", "--name", $allowedName, "--workdir", $system32, "--allow-network", "--",
        $curl
    ) + $allowedCurlArguments
    $allowedResult = Invoke-BoundedProcess $script:WboxPath $allowedArguments $TimeoutSeconds
    Assert-ReachedEndpoint "WNET.3 --allow-network endpoint reachable" $allowedResult

    Assert-NoRunState $runNames
    $stateChecked = $true
}
finally {
    try {
        if ($null -ne $script:WboxPath -and -not $stateChecked) {
            try {
                Assert-NoRunState $runNames
            }
            catch {
                Write-Error $_
            }
        }
    }
    finally {
        Remove-Item -LiteralPath $sandbox -Recurse -Force -ErrorAction SilentlyContinue
    }
}

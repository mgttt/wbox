[CmdletBinding()]
param(
    [ValidateSet("All", "Static", "Rust")]
    [string]$Mode = "All",
    [switch]$WindowsTarget
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$results = [System.Collections.Generic.List[object]]::new()

function Invoke-External {
    param(
        [Parameter(Mandatory)][string]$FilePath,
        [Parameter(Mandatory)][string[]]$Arguments
    )

    & $FilePath @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "'$FilePath $($Arguments -join ' ')' exited with code $LASTEXITCODE"
    }
}

function Invoke-LintPhase {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][scriptblock]$Action
    )

    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    Write-Host "lint: $Name"
    try {
        & $Action
        $timer.Stop()
        $results.Add([pscustomobject]@{
            Phase = $Name
            Result = "PASS"
            Seconds = [Math]::Round($timer.Elapsed.TotalSeconds, 2)
        })
    } catch {
        $timer.Stop()
        $results.Add([pscustomobject]@{
            Phase = $Name
            Result = "FAIL"
            Seconds = [Math]::Round($timer.Elapsed.TotalSeconds, 2)
        })
        throw
    }
}

Push-Location $repoRoot
try {
    if ($Mode -in @("All", "Static")) {
        Invoke-LintPhase "diff whitespace" {
            Invoke-External git @("diff", "--check")
            Invoke-External git @("diff", "--cached", "--check")
        }

        Invoke-LintPhase "PowerShell syntax" {
            foreach ($path in @(git ls-files --cached --others --exclude-standard -- "*.ps1")) {
                $tokens = $null
                $parseErrors = $null
                [System.Management.Automation.Language.Parser]::ParseFile(
                    (Resolve-Path -LiteralPath $path),
                    [ref]$tokens,
                    [ref]$parseErrors
                ) | Out-Null
                if ($parseErrors.Count -gt 0) {
                    $messages = $parseErrors | ForEach-Object {
                        "$($_.Extent.File):$($_.Extent.StartLineNumber): $($_.Message)"
                    }
                    throw ($messages -join [Environment]::NewLine)
                }
            }
        }

        Invoke-LintPhase "JSON syntax" {
            foreach ($path in @(git ls-files --cached --others --exclude-standard -- "*.json")) {
                try {
                    Get-Content -LiteralPath $path -Raw | ConvertFrom-Json | Out-Null
                } catch {
                    throw "${path}: $($_.Exception.Message)"
                }
            }
        }
    }

    if ($Mode -in @("All", "Rust")) {
        Invoke-LintPhase "rustfmt" {
            Invoke-External cargo @("fmt", "--all", "--", "--check")
        }

        Invoke-LintPhase "clippy host target" {
            Invoke-External cargo @(
                "clippy", "--locked", "--workspace", "--all-targets",
                "--message-format", "short", "--", "-D", "warnings"
            )
        }

        if ($WindowsTarget) {
            Invoke-LintPhase "clippy x86_64-pc-windows-msvc" {
                Invoke-External cargo @(
                    "clippy", "--locked", "--workspace", "--all-targets",
                    "--target", "x86_64-pc-windows-msvc",
                    "--message-format", "short", "--", "-D", "warnings"
                )
            }
        }
    }
} finally {
    Pop-Location
    $results | Format-Table -AutoSize
}

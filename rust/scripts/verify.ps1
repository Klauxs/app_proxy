# Runs the checks every commit must pass, in a fixed order, and stops at the
# first failure. Windows-native tests share a bounded process-query budget, so
# the default test run stays single-threaded until that budget is injectable.
param([switch]$SkipTests)
$ErrorActionPreference = 'Stop'
$cargo = Join-Path $PSScriptRoot 'cargo.ps1'

function Invoke-Step([string]$Name, [string[]]$CargoArgs) {
    Write-Host "== $Name"
    & $cargo -CargoArgs $CargoArgs
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAILED: $Name"
        exit $LASTEXITCODE
    }
}

Invoke-Step 'format' @('fmt', '--all', '--', '--check')
Invoke-Step 'clippy' @('clippy', '--workspace', '--all-targets', '--locked', '--', '-D', 'warnings')
if (-not $SkipTests) {
    Invoke-Step 'tests' @('test', '--workspace', '--locked', '--no-fail-fast', '--', '--test-threads=1')
}
Write-Host 'OK'

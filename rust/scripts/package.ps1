$ErrorActionPreference = 'Stop'
$rustRoot = Split-Path -Parent $PSScriptRoot
$buildRoot = Join-Path $rustRoot 'target\package'
$payloadRoot = Join-Path $buildRoot 'release'
$cargo = Join-Path $PSScriptRoot 'cargo.ps1'
# Keep intermediate payload builds away from a developer's running release pair.
# The only distributable is the single setup executable in target\release.
$commit = (& git -C $rustRoot rev-parse --short=12 HEAD).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Cannot determine build revision.' }
$dirty = @(& git -C $rustRoot status --porcelain --untracked-files=normal)
$suffix = if ($dirty.Count -gt 0) { '-dirty' } else { '' }
# The build ID carries the workspace version; Cargo.toml is its only source.
$manifest = Get-Content -LiteralPath (Join-Path $rustRoot 'Cargo.toml') -Raw
if ($manifest -notmatch '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"') { throw 'Cannot determine workspace version.' }
$version = $Matches[1]
$oldPayload = $env:APP_PROXY_PAYLOAD_DIR
$oldBuild = $env:APP_PROXY_BUILD_ID
$oldFlags = $env:RUSTFLAGS
try {
    # A distributed package must not depend on a separately installed VC runtime.
    $env:RUSTFLAGS = "$oldFlags -C target-feature=+crt-static".Trim()
    & $cargo -CargoArgs @('build', '--release', '--locked', '-p', 'app-proxy-app', '--bins', '--target-dir', $buildRoot)
    if ($LASTEXITCODE -ne 0) { throw 'Frontend/backend build failed.' }
    $env:APP_PROXY_PAYLOAD_DIR = $payloadRoot
    $env:APP_PROXY_BUILD_ID = "$version+$commit$suffix"
    & $cargo -CargoArgs @('build', '--release', '--locked', '-p', 'app-proxy-setup', '--features', 'bundle', '--target-dir', $buildRoot)
    if ($LASTEXITCODE -ne 0) { throw 'Setup build failed.' }
    $releaseRoot = Join-Path $rustRoot 'target\release'
    [IO.Directory]::CreateDirectory($releaseRoot) | Out-Null
    $output = Join-Path $releaseRoot 'AppProxy-Setup.exe'
    Copy-Item -LiteralPath (Join-Path $payloadRoot 'AppProxy-Setup.exe') -Destination $output -Force
    [pscustomobject]@{ Path = $output; Bytes = (Get-Item -LiteralPath $output).Length; SHA256 = (Get-FileHash -LiteralPath $output -Algorithm SHA256).Hash } | Format-List
} finally {
    $env:APP_PROXY_PAYLOAD_DIR = $oldPayload
    $env:APP_PROXY_BUILD_ID = $oldBuild
    $env:RUSTFLAGS = $oldFlags
}

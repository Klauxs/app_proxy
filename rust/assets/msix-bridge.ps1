$ErrorActionPreference = 'Stop'
$stage = 'input'
try {
    [Console]::InputEncoding = New-Object System.Text.UTF8Encoding($false)
    [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
    $request = [Console]::In.ReadToEnd() | ConvertFrom-Json
    if ($request.operation -notin @('discover', 'probe')) { throw 'INVALID_OPERATION' }
    $known = @{
        'Claude_pzs8sxrjxfjjc' = 'Claude'
        'OpenAI.Codex_2p2nqsd0c76g0' = 'App'
    }
    if ($request.operation -eq 'probe' -and $known[$request.family_name] -ne $request.app_id) { throw 'PACKAGE_NOT_FULL_TRUST' }
    if ([string]::IsNullOrEmpty($request.family_name) -or $request.family_name.LastIndexOf('_') -lt 1) { throw 'APP_NOT_INSTALLED' }
    $name = $request.family_name.Substring(0, $request.family_name.LastIndexOf('_'))
    $stage = 'package_query'
    $packages = @(Get-AppxPackage -Name $name | Where-Object { $_.PackageFamilyName -eq $request.family_name })
    if ($packages.Count -eq 0) { throw 'APP_NOT_INSTALLED' }
    if ($packages.Count -ne 1) { throw 'AMBIGUOUS_PACKAGE' }
    $package = $packages[0]
    $stage = 'manifest'
    $manifest = Get-AppxPackageManifest -Package $package.PackageFullName
    $apps = @($manifest.Package.Applications.Application | Where-Object {
        $_.Id -eq $request.app_id -and $_.EntryPoint -eq 'Windows.FullTrustApplication'
    })
    if ($apps.Count -ne 1) { throw 'PACKAGE_NOT_FULL_TRUST' }
    $exe = [IO.Path]::GetFullPath((Join-Path $package.InstallLocation ([string]$apps[0].Executable)))
    if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) { throw 'APP_NOT_INSTALLED' }
    if ($request.operation -eq 'discover') {
        $virtualization = $manifest.Package.Properties.SelectSingleNode("*[local-name()='FileSystemWriteVirtualization' and namespace-uri()='http://schemas.microsoft.com/appx/manifest/desktop/windows10/6']")
        $result = @{
            family_name = $package.PackageFamilyName
            full_name = $package.PackageFullName
            app_id = [string]$apps[0].Id
            exe = $exe
            isolated_storage = ($null -eq $virtualization -or $virtualization.InnerText -ne 'disabled')
        }
    } else {
        if ($package.PackageFullName -ne $request.expected_full_name) { throw 'PACKAGE_CHANGED' }
        # Both are real Windows paths: quotes/NUL are invalid; request is a file, not a trailing directory slash.
        foreach ($value in @($request.helper, $request.request)) {
            if (-not [IO.Path]::IsPathRooted($value) -or $value.Contains('"') -or $value.Contains([char]0)) { throw 'INVALID_PATH' }
        }
        if ([IO.Path]::GetFileName($request.helper) -ne 'app-proxy-host.exe') { throw 'INVALID_HELPER' }
        $arguments = 'probe-child --request "' + $request.request + '"'
        $stage = 'activation'
        Invoke-CommandInDesktopPackage -PackageFamilyName $request.family_name -AppId $request.app_id -Command $request.helper -Args $arguments -PreventBreakaway -ErrorAction Stop | Out-Null
        $result = @{ accepted = $true }
    }
    $result | ConvertTo-Json -Compress -Depth 8
} catch {
    $code = [string]$_.Exception.Message
    if ($code -notin @('APP_NOT_INSTALLED','AMBIGUOUS_PACKAGE','PACKAGE_CHANGED','PACKAGE_NOT_FULL_TRUST')) { $code = 'PACKAGE_BRIDGE_FAILED' }
    @{ error = $code; stage = $stage; system_code = $_.Exception.HResult } | ConvertTo-Json -Compress
}

$ErrorActionPreference = 'Stop'
$stage = 'input'
try {
    [Console]::InputEncoding = New-Object System.Text.UTF8Encoding($false)
    [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
    $request = [Console]::In.ReadToEnd() | ConvertFrom-Json
    if ($request.operation -notin @('discover', 'probe', 'launch', 'conflict')) { throw 'INVALID_OPERATION' }
    $known = @{
        'Claude_pzs8sxrjxfjjc' = 'Claude'
        'OpenAI.Codex_2p2nqsd0c76g0' = 'App'
    }
    if ($request.operation -ne 'discover' -and $known[$request.family_name] -ne $request.app_id) { throw 'PACKAGE_NOT_FULL_TRUST' }
    if ($request.operation -eq 'conflict') {
        # A timed-out activation is ambiguous. Only fresh paired runtime events
        # for this package, helper, user and activity prove a container conflict.
        $stage = 'conflict_evidence'
        $start = [DateTimeOffset]::FromUnixTimeMilliseconds([long]$request.started_ms)
        $end = [DateTimeOffset]::FromUnixTimeMilliseconds([long]$request.finished_ms)
        if ($end -lt $start -or ($end - $start).TotalSeconds -gt 20) { throw 'INVALID_EVENT_WINDOW' }
        $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
        $events = @(Get-WinEvent -FilterHashtable @{
            LogName = 'Microsoft-Windows-AppModel-Runtime/Admin'; Id = @(208,215)
            StartTime = $start.LocalDateTime; EndTime = $end.LocalDateTime
        } -MaxEvents 64 -ErrorAction SilentlyContinue | ForEach-Object {
            $xml = [xml]$_.ToXml()
            $fields = @{}
            foreach ($data in $xml.Event.EventData.Data) { $fields[[string]$data.Name] = [string]$data.'#text' }
            [pscustomobject]@{ Id = $_.Id; Activity = [string]$xml.Event.System.Correlation.ActivityID; Fields = $fields }
        })
        $confirmed = $false
        foreach ($event in $events) {
            if ($event.Id -ne 208 -or [string]::IsNullOrEmpty($event.Activity) -or
                $event.Fields.PackageName -cne $request.expected_full_name -or
                $event.Fields.ApplicationName -cne ($request.family_name + '!' + $request.app_id) -or
                $event.Fields.ImageName -ine 'app-proxy-host.exe' -or $event.Fields.ErrorCode -ne '2147942432') { continue }
            foreach ($container in $events) {
                if ($container.Id -eq 215 -and $container.Activity -eq $event.Activity -and
                    $container.Fields.PackageName -ceq $request.expected_full_name -and
                    $container.Fields.ContainerName -ceq ($request.expected_full_name + '-' + $sid) -and
                    $container.Fields.ErrorCode -eq '2147942432') { $confirmed = $true }
            }
        }
        @{ confirmed = $confirmed } | ConvertTo-Json -Compress
        return
    }
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
        $entry = if ($request.operation -eq 'launch') { 'package-child' } else { 'probe-child' }
        $arguments = $entry + ' --request "' + $request.request + '"'
        $stage = 'activation'
        Invoke-CommandInDesktopPackage -PackageFamilyName $request.family_name -AppId $request.app_id -Command $request.helper -Args $arguments -PreventBreakaway -ErrorAction Stop | Out-Null
        $result = @{ accepted = $true }
    }
    $result | ConvertTo-Json -Compress -Depth 8
} catch {
    $code = [string]$_.Exception.Message
    # PowerShell may wrap the activation COM exception. Keep its HRESULT so
    # only a proven sharing conflict can enter the stale-container recovery.
    $systemCode = $_.Exception.HResult
    $exception = $_.Exception
    while ($null -ne $exception) {
        if ($exception.HResult -eq -2147024864) { $systemCode = $exception.HResult; break }
        $exception = $exception.InnerException
    }
    if ($code -notin @('APP_NOT_INSTALLED','AMBIGUOUS_PACKAGE','PACKAGE_CHANGED','PACKAGE_NOT_FULL_TRUST')) { $code = 'PACKAGE_BRIDGE_FAILED' }
    @{ error = $code; stage = $stage; system_code = $systemCode } | ConvertTo-Json -Compress
}

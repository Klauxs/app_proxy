$ErrorActionPreference = 'Stop'
[Console]::InputEncoding = New-Object System.Text.UTF8Encoding($false)
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$currentSid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$currentSession = (Get-Process -Id $PID).SessionId
. (Join-Path $PSScriptRoot 'events-task.ps1')
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class AppProxyArgs {
  [DllImport("shell32.dll", SetLastError=true)] static extern IntPtr CommandLineToArgvW([MarshalAs(UnmanagedType.LPWStr)] string value, out int count);
  [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr ptr);
  [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr OpenProcess(uint access, bool inherit, int pid);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetProcessTimes(IntPtr process, out long created, out long exited, out long kernel, out long user);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool GetTokenInformation(IntPtr token, int type, IntPtr info, int length, out int required);
  public static bool IsOwned(int pid, string sid, long expectedCreated) {
    IntPtr process = OpenProcess(0x1000, false, pid);
    if (process == IntPtr.Zero) return false;
    try {
      long created, exited, kernel, user;
      // CIM reports microsecond precision; reject a recycled PID before reading its token.
      if (!GetProcessTimes(process, out created, out exited, out kernel, out user) || created / 10 != expectedCreated / 10) return false;
      IntPtr token;
      if (!OpenProcessToken(process, 8, out token)) return false;
      try {
        int required; GetTokenInformation(token, 1, IntPtr.Zero, 0, out required);
        if (required <= 0 || required > 65536) return false;
        IntPtr data = Marshal.AllocHGlobal(required);
        try {
          if (!GetTokenInformation(token, 1, data, required, out required)) return false;
          return new System.Security.Principal.SecurityIdentifier(Marshal.ReadIntPtr(data)).Value == sid;
        } finally { Marshal.FreeHGlobal(data); }
      } finally { CloseHandle(token); }
    } finally { CloseHandle(process); }
  }
  public static string[] Parse(string value) {
    if (String.IsNullOrEmpty(value)) return new string[0];
    int count; IntPtr ptr = CommandLineToArgvW(value, out count);
    if (ptr == IntPtr.Zero) return new string[0];
    try { string[] result = new string[count]; for(int i=0;i<count;i++) result[i]=Marshal.PtrToStringUni(Marshal.ReadIntPtr(ptr,i*IntPtr.Size)); return result; }
    finally { LocalFree(ptr); }
  }
}
'@
$storeLocks = @{}
function Get-Identity($item) {
  if (-not $item -or -not $item.ExecutablePath) { return $null }
  $owned = $item.SessionId -eq $currentSession -and [AppProxyArgs]::IsOwned([int]$item.ProcessId, $currentSid, $item.CreationDate.ToFileTimeUtc())
  return @{ pid=[int]$item.ProcessId; created=$item.CreationDate.ToUniversalTime().ToString('o'); path=$item.ExecutablePath; args=@([AppProxyArgs]::Parse($item.CommandLine)); parent=[int]$item.ParentProcessId; session=[int]$item.SessionId; owned=$owned }
}
function Get-RegisteredPackage($family) {
  if (-not $family -or $family.LastIndexOf('_') -le 0) { return @() }
  $name = $family.Substring(0, $family.LastIndexOf('_'))
  return @(Get-AppxPackage -Name $name | Where-Object { $_.PackageFamilyName -eq $family })
}
function Find-Identity($target) { Get-Identity (Get-CimInstance Win32_Process -Filter "ProcessId=$([int]$target)" -ErrorAction SilentlyContinue) }
function Assert-Identity($expected) {
  $actual = Find-Identity $expected.pid
  if (-not $actual) { return $null }
  if (-not $actual.owned -or $actual.created -ne $expected.created -or $actual.path -ine $expected.path) { throw 'Process identity changed; refusing to stop' }
  return $actual
}
while ($null -ne ($line = [Console]::ReadLine())) {
  $request = $null
  try {
    $request = $line | ConvertFrom-Json
    $result = $null
    switch ($request.op) {
      'events-status' { $result = Get-EventsStatus (Get-EventsSpec $request.key $currentSid) }
      'events-start' {
        $spec = Get-EventsSpec $request.key $currentSid
        $task = Get-EventsTask $spec
        if (-not $task) { throw 'Elevated listener is not installed' }
        if ($task.State -ne 4) { $null = $task.RunEx($null, 4, $currentSession, $null) }
        $result = Get-EventsStatus $spec
      }
      { $_ -in @('events-install','events-remove') } {
        $spec = Get-EventsSpec $request.key $currentSid
        $status = Get-EventsStatus $spec
        if (($request.op -eq 'events-install' -and -not $status.current) -or ($request.op -eq 'events-remove' -and $status.installed)) {
          $installer = Join-Path $PSScriptRoot 'install-events.ps1'
          $action = if ($request.op -eq 'events-install') { 'Install' } else { 'Remove' }
          $arguments = "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$installer`" -Action $action -Key $($spec.key) -UserSid $currentSid"
          try {
            $setup = Start-Process -FilePath $spec.ps -ArgumentList $arguments -Verb RunAs -WindowStyle Hidden -PassThru
          } catch { throw 'Windows administrator authorization was cancelled or unavailable; scanning remains available' }
          if (-not $setup.WaitForExit(120000)) { throw 'Administrator setup has not finished; check listener status before retrying' }
          if ($setup.ExitCode -ne 0) { throw 'Administrator listener setup failed; use the same Windows administrator account' }
        }
        $result = Get-EventsStatus $spec
      }
      'singbox-discover' {
        $items = @(Get-CimInstance Win32_Process -Filter "Name='sing-box.exe'" | Where-Object { $_.ExecutablePath })
        $paths = @((Get-Command sing-box.exe -CommandType Application -All -ErrorAction SilentlyContinue | ForEach-Object { $_.Source }))
        $paths += @($items | ForEach-Object { $_.ExecutablePath })
        foreach ($candidate in @((Join-Path $env:USERPROFILE 'scoop\apps\sing-box\current\sing-box.exe'), (Join-Path $env:LOCALAPPDATA 'Microsoft\WinGet\Links\sing-box.exe'))) {
          if (Test-Path -LiteralPath $candidate -PathType Leaf) { $paths += $candidate }
        }
        $listeners = @()
        if ($items.Count) {
          $connections = @(Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue)
          $listeners = @(foreach ($item in $items) {
            foreach ($connection in $connections | Where-Object { $_.OwningProcess -eq $item.ProcessId -and $_.LocalAddress -in @('127.0.0.1','0.0.0.0','::1','::') }) {
              @{ host=$(if ($connection.LocalAddress.Contains(':')) {'::1'} else {'127.0.0.1'}); port=[int]$connection.LocalPort; path=[string]$item.ExecutablePath; pid=[int]$item.ProcessId; created=$item.CreationDate.ToUniversalTime().ToString('o') }
            }
          })
        }
        $result = @{ binaries=@($paths | Select-Object -Unique); listeners=$listeners }
      }
      'lock-acquire' {
        $result = $false
        if (-not $storeLocks.ContainsKey($request.path)) {
          try {
            $stream = [IO.File]::Open($request.path, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
            $storeLocks[$request.path] = @{ stream=$stream; token=$request.token }
            $result = $true
          } catch [IO.IOException] {
            if (($_.Exception.HResult -band 0xffff) -notin @(32,33)) { throw }
          }
        }
      }
      'lock-release' {
        $held = $storeLocks[$request.path]
        if (-not $held -or $held.token -ne $request.token) { throw 'Store lock ownership mismatch' }
        $held.stream.Dispose()
        $storeLocks.Remove($request.path)
        $result = $true
      }
      'network-adapters' {
        $adapters = @(Get-NetAdapter -IncludeHidden)
        $addresses = @(Get-NetIPAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue)
        $routes = @(Get-NetRoute -AddressFamily IPv4 -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue)
        $interfaces = @(Get-NetIPInterface -AddressFamily IPv4 -ErrorAction SilentlyContinue)
        $result = @(foreach ($adapter in $adapters) {
          $index = [int]$adapter.ifIndex
          $metric = $null
          $ipInterface = $interfaces | Where-Object { $_.InterfaceIndex -eq $index } | Select-Object -First 1
          $route = $routes | Where-Object { $_.InterfaceIndex -eq $index } | Sort-Object RouteMetric | Select-Object -First 1
          if ($route -and $ipInterface) { $metric = [int]$route.RouteMetric + [int]$ipInterface.InterfaceMetric }
          @{ name=[string]$adapter.Name; index=$index; up=($adapter.Status -eq 'Up'); hardware=[bool]$adapter.HardwareInterface; virtual=[bool]$adapter.Virtual;
             wifi=([int]$adapter.NdisPhysicalMedium -in @(1,9)); metric=$metric;
             addresses=@($addresses | Where-Object { $_.InterfaceIndex -eq $index -and $_.AddressState -eq 'Preferred' } | ForEach-Object { [string]$_.IPAddress }) }
        })
      }
      'identity' { $result = Find-Identity $request.target }
      'physical-file' {
        if (-not ('AppProxyIcons' -as [type])) { Add-Type -Path (Join-Path $PSScriptRoot 'icons.cs') }
        $result = [AppProxyIcons]::PhysicalFilePath($request.path)
      }
      'package-resolve' {
        $result = $null
        $packages = if ($request.familyName) { @(Get-RegisteredPackage $request.familyName) } else { @(Get-AppxPackage | Where-Object { $_.InstallLocation }) }
        foreach ($package in $packages) {
          if (-not $request.familyName -and -not ([string]$request.exe).StartsWith($package.InstallLocation.TrimEnd('\')+'\', [StringComparison]::OrdinalIgnoreCase)) { continue }
          $manifest = Get-AppxPackageManifest -Package $package.PackageFullName
          foreach ($application in $manifest.Package.Applications.Application) {
            if (-not $application.Executable) { continue }
            $exe = [IO.Path]::GetFullPath((Join-Path $package.InstallLocation ([string]$application.Executable)))
            if (($request.familyName -and $application.Id -eq $request.appId) -or (-not $request.familyName -and $exe -ieq $request.exe)) {
              $virtualization = $manifest.Package.Properties.SelectSingleNode("*[local-name()='FileSystemWriteVirtualization' and namespace-uri()='http://schemas.microsoft.com/appx/manifest/desktop/windows10/6']")
              $result = @{ familyName=$package.PackageFamilyName; appId=[string]$application.Id; exe=$exe; aumid=$package.PackageFamilyName+'!'+$application.Id; fullTrust=($application.EntryPoint -eq 'Windows.FullTrustApplication'); isolatedStorage=($null -eq $virtualization -or $virtualization.InnerText -ne 'disabled') }
            }
          }
        }
      }
      'package-launch' {
        $package = @(Get-RegisteredPackage $request.familyName)
        if ($package.Count -ne 1) { throw 'Package not installed for current user' }
        $manifest = Get-AppxPackageManifest -Package $package[0].PackageFullName
        $application = @($manifest.Package.Applications.Application | Where-Object { $_.Id -eq $request.appId -and $_.EntryPoint -eq 'Windows.FullTrustApplication' })
        if ($application.Count -ne 1) { throw 'Only registered full-trust desktop applications are supported' }
        Invoke-CommandInDesktopPackage -PackageFamilyName $request.familyName -AppId $request.appId -Command $request.node -Args $request.arguments -PreventBreakaway -ErrorAction Stop | Out-Null
        $result = $true
      }
      'processes' {
        $items = Get-CimInstance Win32_Process -Filter "SessionId=$currentSession"
        $result = @(foreach ($item in $items) {
          if ($item.ExecutablePath -and (($request.paths.Count -eq 0) -or ($request.paths -icontains $item.ExecutablePath))) {
            $identity = Get-Identity $item
            if ($identity.owned) { $identity }
          }
        })
      }
      'stop' {
        $actual = Assert-Identity $request.identity
        if ($actual) {
          $process = Get-Process -Id $actual.pid -ErrorAction SilentlyContinue
          if ($process) {
            try {
              # Keep an OS handle open, so a recycled PID can never be our kill target.
              $null = $process.Handle
              $actual = Assert-Identity $request.identity
              if ($actual -and -not $process.HasExited) {
                $null = $process.CloseMainWindow(); $null = $process.WaitForExit(1500)
                if (-not $process.HasExited) { $process.Kill(); $null = $process.WaitForExit(3000) }
              }
            } finally { $process.Dispose() }
          }
        }
        $result = $true
      }
      'protect' {
        $acl = New-Object System.Security.AccessControl.DirectorySecurity
        $acl.SetAccessRuleProtection($true, $false)
        $acl.SetOwner((New-Object System.Security.Principal.SecurityIdentifier($currentSid)))
        foreach ($sid in @($currentSid, 'S-1-5-18')) {
          $rule = New-Object System.Security.AccessControl.FileSystemAccessRule((New-Object System.Security.Principal.SecurityIdentifier($sid)), 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
          $acl.AddAccessRule($rule)
        }
        Set-Acl -LiteralPath $request.path -AclObject $acl
        $result = $true
      }
      'shortcut' {
        if (-not ('AppProxyIcons' -as [type])) { Add-Type -Path (Join-Path $PSScriptRoot 'icons.cs') }
        $cachedIcon = $null
        if ($request.iconSource -and $request.iconPath) {
          $bytes = [AppProxyIcons]::Extract($request.iconSource)
          $sha = [Security.Cryptography.SHA256]::Create()
          try { $digest = [BitConverter]::ToString($sha.ComputeHash($bytes)).Replace('-', '').Substring(0,16).ToLowerInvariant() } finally { $sha.Dispose() }
          # A new content address prevents Explorer from reusing a stale icon entry.
          $cachedIcon = Join-Path ([IO.Path]::GetDirectoryName($request.iconPath)) ([IO.Path]::GetFileNameWithoutExtension($request.iconPath) + '.' + $digest + '.ico')
          $temporaryIcon = $cachedIcon + '.' + [Guid]::NewGuid().ToString('N') + '.tmp'
          try {
            $stream = [IO.File]::Open($temporaryIcon, [IO.FileMode]::CreateNew)
            try { $stream.Write($bytes,0,$bytes.Length) } finally { $stream.Dispose() }
            if ([IO.File]::Exists($cachedIcon)) { [IO.File]::Replace($temporaryIcon, $cachedIcon, [NullString]::Value) }
            else { [IO.File]::Move($temporaryIcon, $cachedIcon) }
          } finally {
            if ([IO.File]::Exists($temporaryIcon)) { [IO.File]::Delete($temporaryIcon) }
          }
        }
        $shell = New-Object -ComObject WScript.Shell
        $link = $shell.CreateShortcut($request.path)
        $link.TargetPath = $request.target
        $link.Arguments = $request.arguments
        $link.WorkingDirectory = $request.cwd
        $link.Description = 'App Proxy managed shortcut'
        if ($cachedIcon) { $link.IconLocation = [AppProxyIcons]::PhysicalFilePath($cachedIcon) + ',0' }
        $link.Save()
        [AppProxyIcons]::Notify($request.path)
        $result = $request.path
      }
      'shortcut-remove' {
        if (Test-Path -LiteralPath $request.path) {
          $shell = New-Object -ComObject WScript.Shell
          $link = $shell.CreateShortcut($request.path)
          if ($link.Description -ne 'App Proxy managed shortcut' -or $link.TargetPath -ine $request.target -or $link.Arguments -ne $request.arguments) { throw 'Shortcut changed; keep it' }
          Remove-Item -LiteralPath $request.path
        }
        $result = $true
      }
      'desktop' { $result = [Environment]::GetFolderPath('Desktop') }
      'alert' { $shell = New-Object -ComObject WScript.Shell; $null = $shell.Popup($request.message, 0, 'App Proxy', 16); $result = $true }
      'extract' {
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $zip = [IO.Compression.ZipFile]::OpenRead($request.zip)
        try {
          $entries = @($zip.Entries | Where-Object { $_.Name -eq 'sing-box.exe' })
          if ($entries.Count -ne 1 -or $entries[0].Length -gt 200MB) { throw 'Unexpected release archive' }
          [IO.Compression.ZipFileExtensions]::ExtractToFile($entries[0], $request.destination, $false)
        } finally { $zip.Dispose() }
        $result = $true
      }
      'task-install' {
        $service = New-Object -ComObject Schedule.Service
        $service.Connect()
        $folder = $service.GetFolder('\')
        try { $old = $folder.GetTask($request.name) } catch { $old = $null }
        if ($old -and $old.Definition.RegistrationInfo.Description -ne $request.description) { throw 'Task name belongs to another program' }
        $task = $service.NewTask(0)
        $task.RegistrationInfo.Description = $request.description
        $task.Principal.UserId = $currentSid
        $task.Principal.LogonType = 3
        $task.Principal.RunLevel = 0
        $task.Settings.Enabled = $true
        $task.Settings.ExecutionTimeLimit = 'PT0S'
        $task.Settings.DisallowStartIfOnBatteries = $false
        $task.Settings.StopIfGoingOnBatteries = $false
        $task.Settings.MultipleInstances = 2
        $trigger = $task.Triggers.Create(9)
        $trigger.UserId = $currentSid
        $action = $task.Actions.Create(0)
        $action.Path = $request.target
        $action.Arguments = $request.arguments
        $action.WorkingDirectory = $request.cwd
        $null = $folder.RegisterTaskDefinition($request.name, $task, 6, $null, $null, 3)
        $result = $true
      }
      'task-remove' {
        $service = New-Object -ComObject Schedule.Service
        $service.Connect(); $folder = $service.GetFolder('\')
        try { $old = $folder.GetTask($request.name) } catch { $old = $null }
        if ($old) {
          if ($old.Definition.RegistrationInfo.Description -ne $request.description) { throw 'Task ownership mismatch' }
          $folder.DeleteTask($request.name, 0)
        }
        $result = $true
      }
      default { throw 'Unknown Windows operation' }
    }
    [Console]::WriteLine((@{id=$request.id;ok=$true;result=$result} | ConvertTo-Json -Depth 20 -Compress))
  } catch {
    [Console]::WriteLine((@{id=$request.id;ok=$false;error=$_.Exception.Message} | ConvertTo-Json -Depth 5 -Compress))
  }
}

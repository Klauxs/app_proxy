# Shared by the ordinary bridge and the UAC installer. Never dot-sourced by the installed listener.
function Get-EventsSpec([string]$key, [string]$sid) {
  if ($key -cnotmatch '^[a-f0-9]{12}$' -or $sid -notmatch '^S-1-5-21-[0-9-]+$') { throw 'Invalid listener identity' }
  $root = Join-Path ([Environment]::GetFolderPath('ProgramFiles')) 'AppProxyGuardEvents'
  $directory = Join-Path (Join-Path $root $sid) $key
  $script = Join-Path $directory 'listener.ps1'
  $ps = Join-Path ([Environment]::GetFolderPath('Windows')) 'System32\WindowsPowerShell\v1.0\powershell.exe'
  return @{ root=$root; directory=$directory; script=$script; ps=$ps; key=$key; sid=$sid;
    name="AppProxy-Events-$sid-$key"; description="App Proxy process notifications: $sid/$key";
    arguments="-NoLogo -NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$script`" -Key $key";
    pipe="\\.\pipe\AppProxy-Events-$sid-$key-$((Get-Process -Id $PID).SessionId)" }
}
function Get-EventsTask($spec) {
  $scheduler = New-Object -ComObject Schedule.Service
  $scheduler.Connect()
  $folder = $scheduler.GetFolder('\')
  try { $task = $folder.GetTask($spec.name) } catch {
    if (($_.Exception.HResult -band 0xffff) -eq 2) { return $null }
    throw
  }
  $definition = $task.Definition
  $principalSid = $definition.Principal.UserId
  if ($principalSid -notmatch '^S-1-') { $principalSid = (New-Object Security.Principal.NTAccount($principalSid)).Translate([Security.Principal.SecurityIdentifier]).Value }
  if ($definition.RegistrationInfo.Description -ne $spec.description -or $principalSid -ne $spec.sid -or
      $definition.Principal.RunLevel -ne 1 -or $definition.Principal.LogonType -ne 3 -or $definition.Actions.Count -ne 1 -or
      $definition.Actions.Item(1).Path -ine $spec.ps -or $definition.Actions.Item(1).Arguments -cne $spec.arguments) {
    throw 'Listener task does not match this installation'
  }
  return $task
}
function Get-EventsStatus($spec) {
  $task = Get-EventsTask $spec
  $current = $false
  if ($task -and (Test-Path -LiteralPath $spec.script -PathType Leaf)) {
    $current = (Get-FileHash -LiteralPath $spec.script -Algorithm SHA256).Hash -eq (Get-FileHash -LiteralPath (Join-Path $PSScriptRoot 'elevated-events.ps1') -Algorithm SHA256).Hash
  }
  return @{ installed=($null -ne $task); current=$current; running=($task -and $task.State -eq 4); task=$spec.name; pipe=$spec.pipe }
}

param(
  [Parameter(Mandatory=$true)][ValidateSet('Install','Remove')][string]$Action,
  [Parameter(Mandatory=$true)][ValidatePattern('^[a-f0-9]{12}$')][string]$Key,
  [Parameter(Mandatory=$true)][string]$UserSid
)
$ErrorActionPreference = 'Stop'
try {
  $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
  $principal = New-Object Security.Principal.WindowsPrincipal($identity)
  if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator) -or $identity.User.Value -ne $UserSid) {
    throw 'Authorize using the same Windows account with administrator membership'
  }
  . (Join-Path $PSScriptRoot 'events-task.ps1')
  $spec = Get-EventsSpec $Key $UserSid
  # All elevated writes stay in fixed Program Files directories; reject redirected ancestors.
  foreach ($path in @($spec.root, (Split-Path -Parent $spec.directory), $spec.directory, $spec.script)) {
    if ((Test-Path -LiteralPath $path) -and ((Get-Item -LiteralPath $path -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Redirected listener installation path' }
  }
  $scheduler = New-Object -ComObject Schedule.Service
  $scheduler.Connect(); $folder = $scheduler.GetFolder('\')
  $old = Get-EventsTask $spec
  if ($old) { $old.Stop(0) }
  # Task termination skips finally blocks. Remove only this user's/store's owned ETW session.
  & $spec.ps -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'elevated-events.ps1') -Key $Key -StopTrace
  if ($LASTEXITCODE -ne 0) { throw 'Unable to clean up the owned ETW session' }
  if ($Action -eq 'Remove') {
    if ($old) { $old.Stop(0); $folder.DeleteTask($spec.name, 0) }
    if (Test-Path -LiteralPath $spec.script) { Remove-Item -LiteralPath $spec.script -Force }
    exit 0
  }
  foreach ($path in @($spec.root, (Split-Path -Parent $spec.directory), $spec.directory)) {
    $null = New-Item -ItemType Directory -Path $path -Force
    $acl = New-Object Security.AccessControl.DirectorySecurity
    $acl.SetAccessRuleProtection($true, $false)
    $acl.SetOwner((New-Object Security.Principal.SecurityIdentifier('S-1-5-32-544')))
    foreach ($entry in @(@('S-1-5-18','FullControl'), @('S-1-5-32-544','FullControl'), @('S-1-5-32-545','ReadAndExecute'))) {
      $acl.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule((New-Object Security.Principal.SecurityIdentifier($entry[0])), $entry[1], 'ContainerInherit,ObjectInherit', 'None', 'Allow')))
    }
    Set-Acl -LiteralPath $path -AclObject $acl
  }
  $temp = Join-Path $spec.directory ([Guid]::NewGuid().ToString() + '.tmp')
  $backup = Join-Path $spec.directory ([Guid]::NewGuid().ToString() + '.bak')
  try {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'elevated-events.ps1') -Destination $temp
    if ($old) { $old.Stop(0) }
    if (Test-Path -LiteralPath $spec.script) { [IO.File]::Replace($temp, $spec.script, $backup) } else { [IO.File]::Move($temp, $spec.script) }
  } finally {
    if (Test-Path -LiteralPath $temp) { Remove-Item -LiteralPath $temp }
    if (Test-Path -LiteralPath $backup) { Remove-Item -LiteralPath $backup }
  }
  $fileAcl = New-Object Security.AccessControl.FileSecurity
  $fileAcl.SetAccessRuleProtection($true, $false)
  $fileAcl.SetOwner((New-Object Security.Principal.SecurityIdentifier('S-1-5-32-544')))
  foreach ($entry in @(@('S-1-5-18','FullControl'), @('S-1-5-32-544','FullControl'), @('S-1-5-32-545','ReadAndExecute'))) {
    $fileAcl.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule((New-Object Security.Principal.SecurityIdentifier($entry[0])), $entry[1], 'Allow')))
  }
  Set-Acl -LiteralPath $spec.script -AclObject $fileAcl
  $task = $scheduler.NewTask(0)
  $task.RegistrationInfo.Description = $spec.description
  $task.Principal.UserId = $UserSid
  $task.Principal.LogonType = 3
  $task.Principal.RunLevel = 1
  $task.Settings.Enabled = $true
  $task.Settings.AllowDemandStart = $true
  $task.Settings.ExecutionTimeLimit = 'PT0S'
  $task.Settings.DisallowStartIfOnBatteries = $false
  $task.Settings.StopIfGoingOnBatteries = $false
  $task.Settings.MultipleInstances = 2
  # The ordinary Guard's login task starts this task on demand. No second login trigger.
  $actionDefinition = $task.Actions.Create(0)
  $actionDefinition.Path = $spec.ps
  $actionDefinition.Arguments = $spec.arguments
  $actionDefinition.WorkingDirectory = $spec.directory
  # Ordinary user can read and run, but cannot change elevated task actions.
  $sddl = "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;GRGX;;;$UserSid)"
  # TASK_DONT_ADD_PRINCIPAL_ACE avoids an automatic writable ACE for the ordinary user.
  $null = $folder.RegisterTaskDefinition($spec.name, $task, 22, $UserSid, $null, 3, $sddl)
  exit 0
} catch { Write-Error $_; exit 1 }

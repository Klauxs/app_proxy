# Explicit live test only. One UAC consent installs and removes a unique test task.
param([string]$Key,[string]$Fixture)
$ErrorActionPreference='Stop'
$installer=Join-Path $PSScriptRoot '..\native\install-events.ps1'
$sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$ps=Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
$errorMessage=$null
try {
  & $ps -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $installer -Action Install -Key $Key -UserSid $sid *> (Join-Path $Fixture 'install.log')
  if($LASTEXITCODE -ne 0){throw 'Install failed'}
  Set-Content -LiteralPath (Join-Path $Fixture 'ready') -Value 'ready'
  $until=[DateTime]::UtcNow.AddSeconds(90)
  while(-not (Test-Path -LiteralPath (Join-Path $Fixture 'done')) -and [DateTime]::UtcNow -lt $until){Start-Sleep -Milliseconds 100}
}catch{$errorMessage=$_.Exception.Message}
finally {
  & $ps -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $installer -Action Remove -Key $Key -UserSid $sid *> (Join-Path $Fixture 'remove.log')
  @{error=$errorMessage;removeExit=$LASTEXITCODE}|ConvertTo-Json|Set-Content -LiteralPath (Join-Path $Fixture 'cleanup.json') -Encoding UTF8
}

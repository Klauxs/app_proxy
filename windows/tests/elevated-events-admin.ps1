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
  . (Join-Path $PSScriptRoot '..\native\events-task.ps1')
  $spec=Get-EventsSpec $Key $sid
  $traceName="AppProxy.ProcessStart.$sid.$Key"
  $until=[DateTime]::UtcNow.AddSeconds(150)
  while(-not (Test-Path -LiteralPath (Join-Path $Fixture 'done')) -and [DateTime]::UtcNow -lt $until){
    if((Test-Path -LiteralPath (Join-Path $Fixture 'measure')) -and -not (Test-Path -LiteralPath (Join-Path $Fixture 'metrics.json'))){
      $helper=Get-CimInstance Win32_Process -Filter "Name='powershell.exe'" | Where-Object { $_.CommandLine -like ('*'+$spec.script+'*') -and $_.CommandLine -like ('*-Key '+$Key+'*') } | Select-Object -First 1
      if(-not $helper){throw 'Listener process not found for CPU sample'}
      $process=Get-Process -Id $helper.ProcessId
      $cpu=$process.TotalProcessorTime.TotalMilliseconds; $timer=[Diagnostics.Stopwatch]::StartNew()
      Start-Sleep -Seconds 10
      $process.Refresh()
      $cpuDelta=$process.TotalProcessorTime.TotalMilliseconds-$cpu
      $elapsed=$timer.Elapsed.TotalMilliseconds
      $trace=& logman query $traceName -ets 2>&1
      if($LASTEXITCODE -ne 0){throw 'Owned trace is not running'}
      @{pid=$helper.ProcessId;cpuMs=$cpuDelta;elapsedMs=$elapsed;oneCorePercent=100*$cpuDelta/$elapsed;trace=($trace -join "`n")}|ConvertTo-Json|Set-Content -LiteralPath (Join-Path $Fixture 'metrics.json') -Encoding UTF8
    }
    if((Test-Path -LiteralPath (Join-Path $Fixture 'crash')) -and -not (Test-Path -LiteralPath (Join-Path $Fixture 'crashed'))){
      (Get-EventsTask $spec).Stop(0)
      Set-Content -LiteralPath (Join-Path $Fixture 'crashed') -Value 'crashed'
    }
    Start-Sleep -Milliseconds 100
  }
}catch{$errorMessage=$_.Exception.Message}
finally {
  & $ps -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $installer -Action Remove -Key $Key -UserSid $sid *> (Join-Path $Fixture 'remove.log')
  $removeExit=$LASTEXITCODE
  & logman query "AppProxy.ProcessStart.$sid.$Key" -ets *> $null
  $traceRemoved=$LASTEXITCODE -ne 0
  @{error=$errorMessage;removeExit=$removeExit;traceRemoved=$traceRemoved}|ConvertTo-Json|Set-Content -LiteralPath (Join-Path $Fixture 'cleanup.json') -Encoding UTF8
}

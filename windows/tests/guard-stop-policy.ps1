$ErrorActionPreference = 'Stop'
$tokens = $null; $errors = $null
$source = [IO.File]::ReadAllText((Join-Path $PSScriptRoot '..\native\bridge.ps1'))
$ast = [Management.Automation.Language.Parser]::ParseInput($source,[ref]$tokens,[ref]$errors)
if ($errors.Count) { throw ($errors | Out-String) }
# Exercise the production stop body with a process double; never stop user processes.
$branch = $ast.Find({param($node)
  $node -is [Management.Automation.Language.IfStatementAst] -and
  $node.Clauses[0].Item1.Extent.Text -eq '$actual -and -not $process.HasExited'
}, $true)
if (-not $branch) { throw 'Stop branch not found' }
$bodyText = $branch.Clauses[0].Item2.Extent.Text.Trim()
$body = [ScriptBlock]::Create($bodyText.Substring(1,$bodyText.Length-2))
$cases = @(
  @{name='fresh';age=500;args=@('app.exe');guard=$true;expected='Kill,Wait:3000'},
  @{name='older';age=6000;args=@('app.exe');guard=$true;expected='Close,Wait:500,Kill,Wait:3000'},
  @{name='has-proxy';age=500;args=@('app.exe','--proxy-server=http://127.0.0.1:18099');guard=$true;expected='Close,Wait:500,Kill,Wait:3000'},
  @{name='helper';age=500;args=@('app.exe','--type=utility');guard=$true;expected='Close,Wait:500,Kill,Wait:3000'},
  @{name='unreadable';age=500;args=@();guard=$true;expected='Close,Wait:500,Kill,Wait:3000'},
  @{name='other-stop';age=500;args=@('app.exe');guard=$false;expected='Close,Wait:1500,Kill,Wait:3000'}
)
foreach ($case in $cases) {
  $request = @{guardCorrection=$case.guard}
  $actual = @{created=[DateTime]::UtcNow.AddMilliseconds(-$case.age).ToString('o');args=$case.args}
  $process = [pscustomobject]@{HasExited=$false;Calls=(New-Object 'System.Collections.Generic.List[string]')}
  $process | Add-Member ScriptMethod CloseMainWindow { $this.Calls.Add('Close'); return $true }
  $process | Add-Member ScriptMethod WaitForExit { param($ms) $this.Calls.Add("Wait:$ms"); return $this.HasExited }
  $process | Add-Member ScriptMethod Kill { $this.Calls.Add('Kill'); $this.HasExited=$true }
  . $body
  $observed = $process.Calls -join ','
  if ($observed -ne $case.expected) { throw "$($case.name): $observed" }
  Write-Output "PASS $($case.name): $observed"
}

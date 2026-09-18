$ErrorActionPreference='Stop'
# Compile exactly the installed self-contained payload without invoking its administrator entrypoint.
$source=[IO.File]::ReadAllText((Join-Path $PSScriptRoot '..\native\elevated-events.ps1'))
$tokens=$null; $errors=$null
$ast=[Management.Automation.Language.Parser]::ParseInput($source,[ref]$tokens,[ref]$errors)
if($errors.Count){throw ($errors | Out-String)}
$addType=$ast.Find({param($node) $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq 'Add-Type'},$true)
& ([ScriptBlock]::Create($addType.Extent.Text))
$flags=[Reflection.BindingFlags]::NonPublic
$type=[AppProxyElevatedEvents]
$properties=$type.GetNestedType('Properties',$flags)
if([Runtime.InteropServices.Marshal]::OffsetOf($properties,'LoggerName').ToInt32() -ne 120){throw 'Incorrect EVENT_TRACE_PROPERTIES ABI'}
if([Runtime.InteropServices.Marshal]::SizeOf([Activator]::CreateInstance($type.GetNestedType('LogFile',$flags))) -ne 448){throw 'Incorrect EVENT_TRACE_LOGFILEW ABI'}
Write-Output 'ETW payload compiled; x64 ABI verified'

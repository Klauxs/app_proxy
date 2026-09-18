param([string]$Path,[string]$Ready)
$file=[IO.File]::Open($Path,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
try { [IO.File]::WriteAllText($Ready,'ready'); Start-Sleep -Milliseconds 250 } finally { $file.Dispose() }

param([string]$Archive,[string]$Destination)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [IO.Compression.ZipFile]::OpenRead($Archive)
try {
  foreach ($name in @('sing-box.exe','libcronet.dll','LICENSE')) {
    $entry = @($zip.Entries | Where-Object { $_.Name -ceq $name })
    if ($entry.Count -ne 1 -or $entry[0].Length -gt 200MB) { throw 'Unexpected core release archive' }
    [IO.Compression.ZipFileExtensions]::ExtractToFile($entry[0], (Join-Path $Destination $name), $true)
  }
} finally { $zip.Dispose() }

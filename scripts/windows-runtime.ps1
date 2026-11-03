# Fails when shellrs.exe or shellrs-cli.exe in the given folder needs a DLL
# of the Visual C++ runtime, which Windows leaves to the Redistributable:
# LTSC editions and fresh installs without it cannot start them. The C
# runtime goes into the programs instead (`+crt-static` in
# .cargo/config.toml). Run by the windows and release workflows after they
# build, with the folder the programs are in.
param([Parameter(Mandatory)] [string] $Folder)

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$dumpbin = & $vswhere -latest -products * -find "VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe" |
  Select-Object -First 1
if (-not $dumpbin) {
  Write-Host "No dumpbin.exe: is Visual Studio installed?"
  exit 1
}

$failed = $false
foreach ($program in "shellrs.exe", "shellrs-cli.exe") {
  $path = Join-Path $Folder $program
  $dlls = @(& $dumpbin /nologo /dependents $path |
    ForEach-Object { $_.Trim() } | Where-Object { $_ -match '\.dll$' })
  if ($LASTEXITCODE -ne 0 -or $dlls.Count -eq 0) {
    Write-Host "dumpbin could not read $path"
    exit 1
  }
  # The debug runtime's too: vcruntime140d.dll, ucrtbased.dll.
  $runtime = @($dlls | Where-Object { $_ -match '^(vcruntime|msvcp|concrt|vccorlib|ucrtbased)' })
  if ($runtime.Count -gt 0) {
    Write-Host "$program needs $($runtime -join ', '), which Windows does not ship"
    $failed = $true
  } else {
    Write-Host "$program needs: $($dlls -join ', ')"
  }
}
if ($failed) { exit 1 }

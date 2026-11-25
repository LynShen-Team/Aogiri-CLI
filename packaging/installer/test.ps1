# End-to-end test of install.ps1 (CI: .github/workflows/installers.yml).
#
#   powershell -File packaging/installer/test.ps1 <lynshen.exe> <its version>
#
# Serves a copy of the software library on 127.0.0.1 (the binary under its
# release name, this checkout's installers, a SHA256SUMS as publish.sh writes
# it) and installs the way people do (irm | iex) into a throwaway folder
# (LYNSHEN_INSTALL_DIR). That folder goes on and off the real user PATH, so
# run this on a CI machine.
param(
    [Parameter(Mandatory = $true)][string]$Binary,
    [Parameter(Mandatory = $true)][string]$Version
)
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$file = 'lynshen-x86_64-pc-windows-msvc.exe'
$tmp = [IO.Path]::GetTempPath()
$lib = Join-Path $tmp ('lynshen-lib-' + [guid]::NewGuid())
$cli = Join-Path $lib "cli\$Version"
$bad = Join-Path $lib "bad\$Version"
New-Item -ItemType Directory -Force -Path $cli, $bad | Out-Null
Copy-Item -LiteralPath $Binary -Destination (Join-Path $cli $file)
Copy-Item -Path (Join-Path $here 'install.ps1'), (Join-Path $here 'install.sh') -Destination $cli
Copy-Item -Path (Join-Path $here 'install.ps1') -Destination (Join-Path $lib 'cli')
$sums = Get-ChildItem -LiteralPath $cli -File | Sort-Object Name | ForEach-Object {
    '{0}  {1}' -f (Get-FileHash -Algorithm SHA256 -LiteralPath $_.FullName).Hash.ToLowerInvariant(), $_.Name
}
[IO.File]::WriteAllText((Join-Path $cli 'SHA256SUMS'), (($sums -join "`n") + "`n"))
$release = '{"product":"cli","version":"' + $Version + '","files":[]}'
[IO.File]::WriteAllText((Join-Path $lib 'cli\latest.json'), (@{ key = ''; signature = ''; release = $release } | ConvertTo-Json))
# A release whose SHA256SUMS does not match its binary.
Copy-Item -LiteralPath $Binary -Destination (Join-Path $bad $file)
[IO.File]::WriteAllText((Join-Path $bad 'SHA256SUMS'), ('0' * 64) + "  $file`n")

$server = Start-Process python -ArgumentList '-m', 'http.server', '18471', '--bind', '127.0.0.1', '--directory', $lib -PassThru -WindowStyle Hidden
$base = 'http://127.0.0.1:18471/cli'
foreach ($attempt in 1..20) {
    try { Invoke-WebRequest -UseBasicParsing -Uri "$base/latest.json" | Out-Null; break } catch { Start-Sleep -Milliseconds 500 }
}

$dir = Join-Path $tmp ('lynshen-bin-' + [guid]::NewGuid())
$exe = Join-Path $dir 'lynshen.exe'
$env:LYNSHEN_DOWNLOAD_BASE = $base
$env:LYNSHEN_INSTALL_DIR = $dir
Remove-Item Env:\LYNSHEN_VERSION -ErrorAction SilentlyContinue
$script:failed = 0
$script:passed = 0

function Check([string]$Name, [scriptblock]$Test) {
    $ok = $false
    try { $ok = [bool](& $Test) } catch { Write-Host "     $($_.Exception.Message)" }
    if ($ok) { Write-Host "ok   $Name"; $script:passed++ } else { Write-Host "FAIL $Name"; $script:failed++ }
}
function Show($Lines) { $Lines | ForEach-Object { Write-Host "     $_" } }
function Get-UserPathEntries {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment')
    $value = [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    @($value -split ';' | Where-Object { $_ })
}
function Count-InUserPath { @(Get-UserPathEntries | Where-Object { $_.TrimEnd('\') -ieq $dir }).Count }
# What people type, in this session. Write-Host is stream 6.
function Install-ViaIex { (Invoke-RestMethod "$base/install.ps1" | Invoke-Expression) 6>&1 | ForEach-Object { "$_" } }
# With parameters, as the README shows: & ([scriptblock]::Create((irm ...))) -Uninstall
function Invoke-WithArgs([hashtable]$Arguments) {
    $block = [scriptblock]::Create((Invoke-RestMethod "$base/install.ps1"))
    (& $block @Arguments) 6>&1 | ForEach-Object { "$_" }
}

Write-Host "== PowerShell $($PSVersionTable.PSVersion)"
$out = Install-ViaIex
Show $out
Check 'irm | iex installs lynshen.exe' { Test-Path -LiteralPath $exe }
Check 'this window finds it right away' { (Get-Command lynshen).Source -ieq $exe }
Check 'it runs' { (& lynshen --version) -eq "lynshen $Version" }
Check 'the folder is on the user PATH once' { (Count-InUserPath) -eq 1 }
Check 'the user PATH stays REG_EXPAND_SZ' {
    [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment').GetValueKind('Path') -eq [Microsoft.Win32.RegistryValueKind]::ExpandString
}
Check 'next step: run lynshen' { $out[-1] -eq 'Next: run lynshen' }

$out = Install-ViaIex
Check 'running it again adds nothing to PATH' { (Count-InUserPath) -eq 1 -and @($env:Path -split ';' | Where-Object { $_ -ieq $dir }).Count -eq 1 }
Check 'and says nothing about PATH' { -not ($out -match 'Added') }

$running = Start-Process -FilePath $exe -ArgumentList 'daemon', '--listen', '127.0.0.1:18472', '--no-relay' -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 2
Check 'lynshen daemon is running' { -not $running.HasExited }
$out = Install-ViaIex
Show $out
Check 'upgrading while it runs works' { ($out -match '^Installed lynshen') -and ((& $exe --version) -eq "lynshen $Version") }
Stop-Process -Id $running.Id -Force
$running.WaitForExit()

New-Item -ItemType File -Path (Join-Path $dir 'other-tool.txt') | Out-Null
$out = Invoke-WithArgs @{ Uninstall = $true }
Show $out
Check '-Uninstall removes lynshen.exe' { -not (Test-Path -LiteralPath $exe) -and -not (Test-Path -LiteralPath (Join-Path $dir 'lynshen.old.exe')) }
Check 'a folder with other programs stays on PATH' { (Count-InUserPath) -eq 1 -and ($out -match 'Kept') }
Remove-Item -LiteralPath (Join-Path $dir 'other-tool.txt')
$out = Invoke-WithArgs @{ Uninstall = $true }
Check 'an empty folder leaves PATH' { (Count-InUserPath) -eq 0 -and -not (Test-Path -LiteralPath $dir) }

$out = Invoke-WithArgs @{ Version = $Version; NoModifyPath = $true }
Check '-Version and -NoModifyPath' { (Test-Path -LiteralPath $exe) -and (Count-InUserPath) -eq 0 }
Invoke-WithArgs @{ Uninstall = $true } | Out-Null

$env:LYNSHEN_DOWNLOAD_BASE = ($base -replace '/cli$', '/bad')
$env:LYNSHEN_VERSION = $Version
$out = Install-ViaIex
Show $out
Check 'a checksum mismatch fails' { ($out -join "`n") -match 'checksum mismatch' }
Check 'and leaves nothing behind' { -not (Test-Path -LiteralPath $exe) -and -not (Get-ChildItem -LiteralPath $dir -Force -ErrorAction SilentlyContinue) }
$env:LYNSHEN_DOWNLOAD_BASE = $base
Remove-Item Env:\LYNSHEN_VERSION

$shell = (Get-Process -Id $PID).Path
& $shell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $cli 'install.ps1') -Version 0.0.1 *> $null
Check 'run as a file, a failure exits 1' { $LASTEXITCODE -eq 1 }
$out = & $shell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $cli 'install.ps1') 2>&1 | ForEach-Object { "$_" }
Check 'run as a file, it says to open a new terminal' { $LASTEXITCODE -eq 0 -and $out[-1] -eq 'Next: open a new terminal, then run lynshen' }
Invoke-WithArgs @{ Uninstall = $true } | Out-Null
Check 'clean at the end' { (Count-InUserPath) -eq 0 -and -not (Test-Path -LiteralPath $dir) }

Stop-Process -Id $server.Id -Force
Remove-Item -LiteralPath $lib -Recurse -Force
Write-Host "passed $script:passed, failed $script:failed"
if ($script:failed) { exit 1 }

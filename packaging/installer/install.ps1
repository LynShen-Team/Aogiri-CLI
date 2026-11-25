# LynShen CLI installer for Windows.
#
#   irm https://software.lynshen.org/cli/install.ps1 | iex
#
# Downloads lynshen.exe from the LynShen software library, checks its sha256
# against the release's SHA256SUMS, installs it in %USERPROFILE%\.local\bin and
# adds that folder to your user PATH. No administrator rights needed. Run
# through `iex`, the PowerShell window it ran in can use lynshen right away.
#
# Options: set $env:LYNSHEN_VERSION = '0.8.11' before `irm | iex` to install
# that version, or run the script with parameters:
#
#   & ([scriptblock]::Create((irm https://software.lynshen.org/cli/install.ps1))) -Uninstall
#
#   -Version X.Y.Z   install this version instead of the latest
#   -Upgrade         install the latest version over the current one
#                    (the same as running the installer again)
#   -Uninstall       remove lynshen.exe; the folder leaves your PATH when
#                    nothing else is in it. Settings and sessions in
#                    %USERPROFILE%\.lynshen stay.
#   -NoModifyPath    leave your PATH alone
#
# Environment: LYNSHEN_VERSION, LYNSHEN_INSTALL_DIR (default
# %USERPROFILE%\.local\bin), LYNSHEN_DOWNLOAD_BASE (default
# https://software.lynshen.org/cli).
#
# Not %USERPROFILE%\.lynshen\bin: LynShen Desktop keeps its own copy there and
# updates it with the app.

param(
    [string]$Version = $env:LYNSHEN_VERSION,
    [switch]$Upgrade,
    [switch]$Uninstall,
    [switch]$NoModifyPath
)

# Everything runs inside this function: `iex` runs a script in the caller's
# own session, and the window must stay open (no `exit`) with its settings
# untouched when something fails.
function Invoke-LynShenInstaller {
    param([string]$Version, [bool]$Uninstall, [bool]$NoModifyPath, [bool]$InCallerSession)

    $ErrorActionPreference = 'Stop'
    # Windows PowerShell 5.1 downloads many times slower with a progress bar.
    $ProgressPreference = 'SilentlyContinue'
    # Windows PowerShell 5.1 may not offer TLS 1.2 by default.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $base = 'https://software.lynshen.org/cli'
    if ($env:LYNSHEN_DOWNLOAD_BASE) { $base = $env:LYNSHEN_DOWNLOAD_BASE.TrimEnd('/') }
    $dir = Join-Path $HOME '.local\bin'
    if ($env:LYNSHEN_INSTALL_DIR) { $dir = $env:LYNSHEN_INSTALL_DIR.TrimEnd('\', '/') }
    $exe = Join-Path $dir 'lynshen.exe'
    $old = Join-Path $dir 'lynshen.old.exe'
    $file = 'lynshen-x86_64-pc-windows-msvc.exe'

    function Get-Text([string]$Url) {
        $response = Invoke-WebRequest -UseBasicParsing -Uri $Url
        if ($response.Content -is [byte[]]) { return [Text.Encoding]::UTF8.GetString($response.Content) }
        return [string]$response.Content
    }

    function Test-SameDir([string]$Entry, [string]$Dir) {
        $expanded = [Environment]::ExpandEnvironmentVariables($Entry).Trim().TrimEnd('\')
        return $expanded -ieq $Dir.TrimEnd('\')
    }

    # The user PATH as stored (REG_EXPAND_SZ, %VARIABLES% unexpanded).
    function Get-UserPath {
        $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment')
        if (-not $key) { return '' }
        try { return [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) }
        finally { $key.Close() }
    }

    function Set-UserPath([string]$Value) {
        $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
        try { $key.SetValue('Path', $Value, [Microsoft.Win32.RegistryValueKind]::ExpandString) }
        finally { $key.Close() }
        # Setting a user variable through .NET broadcasts WM_SETTINGCHANGE, so
        # terminals started from now on see the new PATH.
        [Environment]::SetEnvironmentVariable('LYNSHEN_INSTALLER', '1', 'User')
        [Environment]::SetEnvironmentVariable('LYNSHEN_INSTALLER', $null, 'User')
    }

    function Get-OtherLynShen {
        $found = Get-Command lynshen -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($found -and $found.Source -and -not ($found.Source -ieq $exe)) { return $found.Source }
        return $null
    }

    if ($Uninstall) {
        if (Test-Path -LiteralPath $exe) {
            Remove-Item -LiteralPath $exe -Force
            Write-Host "Removed $exe"
        } else {
            Write-Host "No lynshen at $exe"
        }
        Remove-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue
        Get-ChildItem -LiteralPath $dir -Filter '.lynshen.download.*' -Force -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
        $userPath = Get-UserPath
        $entries = @($userPath -split ';' | Where-Object { $_ })
        $inPath = @($entries | Where-Object { Test-SameDir $_ $dir }).Count -gt 0
        # Other programs (Claude Code, for one) install in the same folder;
        # it stays on PATH while anything is in it.
        $empty = -not (Test-Path -LiteralPath $dir) -or -not (Get-ChildItem -LiteralPath $dir -Force | Select-Object -First 1)
        if ($inPath -and $empty) {
            Set-UserPath ((@($entries | Where-Object { -not (Test-SameDir $_ $dir) })) -join ';')
            Remove-Item -LiteralPath $dir -Force -ErrorAction SilentlyContinue
            Write-Host "Removed $dir from your user PATH"
        } elseif ($inPath) {
            Write-Host "Kept $dir on your PATH: other programs are in it"
        }
        Write-Host 'Settings and sessions in %USERPROFILE%\.lynshen were kept; delete that folder to remove them too.'
        $other = Get-OtherLynShen
        if ($other) { Write-Host "Another lynshen is still on your PATH: $other (npm or LynShen Desktop)" }
        return
    }

    if (-not [Environment]::Is64BitOperatingSystem) { throw 'lynshen needs 64-bit Windows' }

    if ($Version) {
        $Version = $Version.Trim().TrimStart('v')
    } else {
        try { $manifest = (Get-Text "$base/latest.json") | ConvertFrom-Json }
        catch { throw "could not read $base/latest.json ($($_.Exception.Message))" }
        $release = $manifest.release | ConvertFrom-Json
        if ($release.product -ne 'cli') { throw "$base/latest.json is not a LynShen CLI release" }
        $Version = [string]$release.version
    }
    if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "not a version: '$Version' (expected something like 0.8.11)" }

    try { $sums = Get-Text "$base/$Version/SHA256SUMS" }
    catch { throw "could not read $base/$Version/SHA256SUMS: no such version, or a version released before this installer (pick a newer one)" }
    $expected = $null
    foreach ($line in ($sums -split "`n")) {
        $parts = @($line.Trim() -split '\s+')
        if ($parts.Count -eq 2 -and ($parts[1] -eq $file -or $parts[1] -eq "*$file")) { $expected = $parts[0].ToLowerInvariant() }
    }
    if ($expected -notmatch '^[0-9a-f]{64}$') { throw "release $Version has no checksum for $file" }

    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    # Download next to the destination, so the final step is a rename.
    $partial = Join-Path $dir ".lynshen.download.$PID.exe"
    try {
        Write-Host "Downloading lynshen $Version ($file)"
        try { Invoke-WebRequest -UseBasicParsing -Uri "$base/$Version/$file" -OutFile $partial }
        catch { throw "download failed: $base/$Version/$file ($($_.Exception.Message))" }
        $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $partial).Hash.ToLowerInvariant()
        if ($actual -ne $expected) { throw "checksum mismatch for $file (expected $expected, got $actual); nothing was installed" }
        try { $reported = (& $partial --version | Out-String).Trim() }
        catch { throw "the downloaded lynshen does not run on this computer ($($_.Exception.Message))" }
        if (($reported -split ' ')[-1] -ne $Version) { throw "the downloaded binary reports '$reported', not $Version" }
        # Windows keeps a running exe locked but lets it be renamed, so a
        # lynshen that is open moves aside first (the CLI's own updater does
        # the same).
        if (Test-Path -LiteralPath $exe) {
            Remove-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue
            Move-Item -LiteralPath $exe -Destination $old -Force
        }
        Move-Item -LiteralPath $partial -Destination $exe -Force
        Remove-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue
    } finally {
        Remove-Item -LiteralPath $partial -Force -ErrorAction SilentlyContinue
    }
    Write-Host "Installed lynshen $Version at $exe"

    $entries = @((Get-UserPath) -split ';' | Where-Object { $_ })
    $inUserPath = @($entries | Where-Object { Test-SameDir $_ $dir }).Count -gt 0
    if ($NoModifyPath) {
        if (-not $inUserPath) { Write-Host "Next: add $dir to your PATH, then run lynshen"; return }
    } elseif (-not $inUserPath) {
        Set-UserPath ((@($entries) + $dir) -join ';')
        Write-Host "Added $dir to your user PATH"
    }
    # This window: `iex` runs here, so lynshen works in it right away.
    if (-not (@($env:Path -split ';' | Where-Object { $_ -and (Test-SameDir $_ $dir) }).Count)) {
        $env:Path = ($env:Path.TrimEnd(';') + ";$dir")
    }
    $other = Get-OtherLynShen
    if ($other) { Write-Host "Note: another lynshen comes first on your PATH: $other" }
    if ($InCallerSession) {
        Write-Host 'Next: run lynshen'
    } else {
        Write-Host 'Next: open a new terminal, then run lynshen'
    }
}

# Run as a file (`powershell -File install.ps1`), the script has a path of its
# own and runs in a process of its own; through `irm | iex` it has neither.
$lynshenInstallerFile = $PSCommandPath -and ([IO.Path]::GetFileName($PSCommandPath) -ieq 'install.ps1')
try {
    Invoke-LynShenInstaller -Version $Version -Uninstall $Uninstall.IsPresent -NoModifyPath $NoModifyPath.IsPresent -InCallerSession (-not $lynshenInstallerFile)
} catch {
    Write-Host "lynshen installer: $($_.Exception.Message)" -ForegroundColor Red
    if ($lynshenInstallerFile) { exit 1 }
}

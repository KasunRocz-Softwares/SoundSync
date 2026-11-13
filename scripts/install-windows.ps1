<#
.SYNOPSIS
  Installs the built SyncSound NSIS installer into a target directory and smoke-tests it.
.DESCRIPTION
  1. Verifies the NSIS artifact exists.
  2. Writes a sentinel profile into per-user app data (never the install dir).
  3. Runs the installer silently with an explicit target directory.
  4. Launches the installed executable, checks it stays running, then closes it.
  5. Optionally re-installs over the existing install (upgrade) and confirms the
     sentinel profile survived.
  Exits non-zero on any failure. Does not touch files outside the target directory
  other than what the installer itself writes (Start menu entry, uninstall key).
#>
param(
    [string]$InstallDir = 'F:\SyncSound',
    [switch]$Upgrade,
    [switch]$Uninstall
)
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

$installer = Get-ChildItem 'target/release/bundle/nsis/*.exe' | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $installer) { throw 'NSIS installer missing; run scripts/build-windows.ps1 first' }
if (Test-Path $InstallDir) {
    $foreign = Get-ChildItem $InstallDir -Force | Where-Object { $_.Name -notin @('SyncSound.exe', 'syncsound-desktop.exe', 'uninstall.exe', 'resources', 'Uninstall SyncSound.lnk') }
    if ($foreign) { throw "Install directory $InstallDir contains unrelated files: $($foreign.Name -join ', '). Refusing to install over them." }
}

$profiles = Join-Path $env:APPDATA 'cloud.syncsound.desktop\profiles'
New-Item -ItemType Directory -Force $profiles | Out-Null
$sentinel = Join-Path $profiles 'Install smoke test.json'
$sentinelJson = '{"schema_version":2,"name":"Install smoke test","mode":"balanced","source":{"kind":"default_render"},"outputs":[]}'
$createdSentinel = $false
if (-not (Test-Path $sentinel)) { Set-Content -Path $sentinel -Value $sentinelJson -Encoding UTF8; $createdSentinel = $true }

function Install-Once {
    Write-Host "Installing $($installer.Name) to $InstallDir"
    $p = Start-Process -FilePath $installer.FullName -ArgumentList '/S', "/D=$InstallDir" -Wait -PassThru
    Write-Host "Installer exit code: $($p.ExitCode)"
    if ($p.ExitCode -ne 0) { throw "Installer failed with exit code $($p.ExitCode)" }
}

function Test-Launch {
    $exe = Get-ChildItem $InstallDir -Filter '*.exe' | Where-Object { $_.Name -ne 'uninstall.exe' } | Select-Object -First 1
    if (-not $exe) { throw "No application executable in $InstallDir" }
    Write-Host "Launching $($exe.FullName)"
    $app = Start-Process -FilePath $exe.FullName -PassThru
    Start-Sleep -Seconds 6
    if ($app.HasExited) { throw "Application exited early with code $($app.ExitCode)" }
    Write-Host "Application running (PID $($app.Id)); closing"
    $null = $app.CloseMainWindow()
    if (-not $app.WaitForExit(5000)) { Stop-Process -Id $app.Id -Force }
}

Install-Once
Test-Launch
if ($Upgrade) {
    Install-Once
    Test-Launch
}
if (-not (Test-Path $sentinel)) { throw 'Profile data in app data was not preserved' }
Write-Host "Profile data preserved: $sentinel"

if ($Uninstall) {
    $uninstaller = Join-Path $InstallDir 'uninstall.exe'
    $p = Start-Process -FilePath $uninstaller -ArgumentList '/S' -Wait -PassThru
    Write-Host "Uninstaller exit code: $($p.ExitCode)"
    Start-Sleep -Seconds 3
    if (Test-Path (Join-Path $InstallDir '*.exe')) { throw 'Application files remain after uninstall' }
    if (-not (Test-Path $sentinel)) { throw 'Uninstall removed user profile data' }
    Write-Host 'Uninstall removed the application and kept user profiles'
}
if ($createdSentinel) { Remove-Item $sentinel -ErrorAction SilentlyContinue }
Write-Host 'Install smoke test passed'

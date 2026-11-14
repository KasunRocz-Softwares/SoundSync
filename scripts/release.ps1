<#
.SYNOPSIS
  Builds a signed SyncSound release and (with -Publish) uploads it to GitHub Releases.
.DESCRIPTION
  1. Optionally bumps the version in tauri.conf.json, package.json and both Cargo.toml files.
  2. Runs scripts/build-windows.ps1 (npm ci, tests, checks, NSIS + MSI) with the updater
     signing key, so the NSIS installer gets a .sig file.
  3. Writes latest.json, which installed copies read to find updates:
     https://github.com/KasunRocz-Softwares/SoundSync/releases/latest/download/latest.json
  4. Also builds the Windows on ARM (arm64) installer when this PC can: it needs
     `rustup target add aarch64-pc-windows-msvc` and Visual Studio's "MSVC ARM64 build tools".
     Without them the release is x64 only (pass -NoArm64 to skip on purpose).
  5. With -Publish: tags v<version>, pushes the tag, and creates the GitHub release with the
     installers, their signatures, the MSIs and latest.json.

  The private signing key never leaves this PC. Default location:
  %USERPROFILE%\.tauri\syncsound-updater.key (create it with: npx tauri signer generate -w <path>).
  Back it up: without it, existing installs can never verify a future update.
.EXAMPLE
  powershell -ExecutionPolicy Bypass -File .\scripts\release.ps1 -Version 0.2.1 -Notes "Fixes" -Publish
#>
param(
    [string]$Version,
    [string]$Notes = 'See the release page for details.',
    [string]$Repo = 'KasunRocz-Softwares/SoundSync',
    [string]$KeyPath = (Join-Path $env:USERPROFILE '.tauri\syncsound-updater.key'),
    [switch]$Publish,
    [switch]$NoArm64
)
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

# Windows PowerShell 5.1's "utf8" adds a byte-order mark, which JSON and TOML readers may reject.
$utf8 = New-Object System.Text.UTF8Encoding($false)
function Write-Utf8([string]$Path, [string]$Text) {
    [System.IO.File]::WriteAllText((Join-Path (Get-Location) $Path), $Text, $utf8)
}

$confPath = 'apps/desktop/src-tauri/tauri.conf.json'
$conf = Get-Content $confPath -Raw | ConvertFrom-Json
if (-not $Version) { $Version = $conf.version }
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Version must look like 1.2.3 (got '$Version')" }

if ($conf.version -ne $Version) {
    Write-Host "==> Bumping version $($conf.version) -> $Version"
    foreach ($file in @($confPath, 'package.json')) {
        $text = Get-Content $file -Raw
        $text = [regex]::Replace($text, '"version":\s*"\d+\.\d+\.\d+"', "`"version`": `"$Version`"", 1)
        Write-Utf8 $file $text
    }
    foreach ($file in @('apps/desktop/src-tauri/Cargo.toml', 'crates/syncsound-engine/Cargo.toml')) {
        $text = Get-Content $file -Raw
        $text = [regex]::Replace($text, '(?m)^version = "\d+\.\d+\.\d+"', "version = `"$Version`"", 1)
        Write-Utf8 $file $text
    }
    cargo update --workspace --quiet
    if ($Publish) {
        git add $confPath package.json apps/desktop/src-tauri/Cargo.toml crates/syncsound-engine/Cargo.toml Cargo.lock
        git commit -m "chore: release v$Version"
        if ($LASTEXITCODE -ne 0) { throw 'Committing the version bump failed' }
    }
}

if (-not (Test-Path $KeyPath)) { throw "Updater signing key not found at $KeyPath" }
$env:TAURI_SIGNING_PRIVATE_KEY = Get-Content $KeyPath -Raw
# PowerShell deletes an environment variable set to '', so Tauri can't see an
# empty password and prompts for one. A key without a password gets an empty
# line on standard input instead; a key with one uses the variable.
$keyPassword = "$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD"

$keyPassword | & powershell -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'build-windows.ps1')
if ($LASTEXITCODE -ne 0) { throw "Build failed with exit code $LASTEXITCODE" }

$setup = Get-Item "target/release/bundle/nsis/SyncSound_${Version}_x64-setup.exe"
$sig = Get-Item "$($setup.FullName).sig"
$msi = Get-Item "target/release/bundle/msi/SyncSound_${Version}_x64_en-US.msi"
$tag = "v$Version"
$manifest = [ordered]@{
    version   = $Version
    notes     = $Notes
    pub_date  = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
    platforms = [ordered]@{
        'windows-x86_64' = [ordered]@{
            signature = (Get-Content $sig.FullName -Raw).Trim()
            url       = "https://github.com/$Repo/releases/download/$tag/$($setup.Name)"
        }
    }
}
$assets = @($setup.FullName, $sig.FullName, $msi.FullName)

# Windows on ARM: cross-built with the same updater key.
$armTarget = 'aarch64-pc-windows-msvc'
$armLibs = @("${env:ProgramFiles(x86)}\Microsoft Visual Studio", "$env:ProgramFiles\Microsoft Visual Studio") |
    Where-Object { Test-Path "$_\*\*\VC\Tools\MSVC\*\lib\arm64" }
$armReady = (-not $NoArm64) -and ((rustup target list --installed) -contains $armTarget) -and $armLibs
if ($armReady) {
    Write-Host "==> Building the Windows on ARM installer"
    $keyPassword | npm run desktop:build -- --target $armTarget
    if ($LASTEXITCODE -ne 0) { throw "ARM64 build failed with exit code $LASTEXITCODE" }
    $armSetup = Get-Item "target/$armTarget/release/bundle/nsis/SyncSound_${Version}_arm64-setup.exe"
    $armSig = Get-Item "$($armSetup.FullName).sig"
    $armMsi = Get-ChildItem "target/$armTarget/release/bundle/msi/SyncSound_${Version}_arm64*.msi" | Select-Object -First 1
    $manifest.platforms['windows-aarch64'] = [ordered]@{
        signature = (Get-Content $armSig.FullName -Raw).Trim()
        url       = "https://github.com/$Repo/releases/download/$tag/$($armSetup.Name)"
    }
    $assets += @($armSetup.FullName, $armSig.FullName)
    if ($armMsi) { $assets += $armMsi.FullName }
} elseif (-not $NoArm64) {
    Write-Warning "Skipping the Windows on ARM installer: install the Rust target ($armTarget) and Visual Studio's MSVC ARM64 build tools to include it."
}

$latest = 'target/release/bundle/latest.json'
Write-Utf8 $latest ($manifest | ConvertTo-Json -Depth 5)
Write-Host "==> Wrote $latest"

if (-not $Publish) {
    Write-Host "Built $tag. Re-run with -Publish to create the GitHub release."
    return
}
if (-not (Get-Command gh -ErrorAction SilentlyContinue)) { throw 'Install the GitHub CLI (gh) and run gh auth login.' }
git push origin HEAD:main
if ($LASTEXITCODE -ne 0) { throw 'Pushing main failed' }
git tag -a $tag -m "SyncSound $Version"
if ($LASTEXITCODE -ne 0) { throw "Tag $tag already exists" }
git push origin $tag
if ($LASTEXITCODE -ne 0) { throw 'Pushing the tag failed' }
gh release create $tag @assets $latest --repo $Repo --title "SyncSound $Version" --notes "$Notes`n`n[Code signing policy](https://github.com/$Repo/blob/main/docs/CODE_SIGNING.md)"
if ($LASTEXITCODE -ne 0) { throw 'Creating the GitHub release failed' }
Write-Host "==> Published https://github.com/$Repo/releases/tag/$tag"

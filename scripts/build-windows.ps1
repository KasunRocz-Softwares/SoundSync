$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { throw 'Install Rust from https://rustup.rs/' }
if (-not (Get-Command npm -ErrorAction SilentlyContinue)) { throw 'Install Node.js 22 LTS or later.' }

function Invoke-Step([string]$Name, [scriptblock]$Command) {
    Write-Host "==> $Name"
    & $Command
    if ($LASTEXITCODE -ne 0) { throw "$Name failed with exit code $LASTEXITCODE" }
}

Invoke-Step 'npm ci' { npm ci }
Invoke-Step 'frontend build' { npm run build }
Invoke-Step 'cargo test' { cargo test --workspace }
Invoke-Step 'cargo check' { cargo check --workspace }
Invoke-Step 'tauri build' { npm run desktop:build }

# Fail when either bundle is missing at the actual Tauri output path.
$nsis = Get-ChildItem 'target/release/bundle/nsis/*.exe' -ErrorAction SilentlyContinue
$msi = Get-ChildItem 'target/release/bundle/msi/*.msi' -ErrorAction SilentlyContinue
if (-not $nsis) { throw 'NSIS installer missing: target/release/bundle/nsis/*.exe' }
if (-not $msi) { throw 'MSI installer missing: target/release/bundle/msi/*.msi' }
@($nsis) + @($msi) | ForEach-Object {
    $hash = (Get-FileHash $_.FullName -Algorithm SHA256).Hash
    Write-Host ("{0}  {1:N0} bytes  SHA256 {2}" -f $_.FullName, $_.Length, $hash)
}

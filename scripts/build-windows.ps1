$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { throw 'Install Rust from https://rustup.rs/' }
if (-not (Get-Command npm -ErrorAction SilentlyContinue)) { throw 'Install Node.js 22 LTS or later.' }
npm ci
if ($LASTEXITCODE -ne 0) { throw 'npm ci failed' }
npm run desktop:build
if ($LASTEXITCODE -ne 0) { throw 'Tauri build failed' }
Write-Host 'Installers are in target/release/bundle/nsis and target/release/bundle/msi'

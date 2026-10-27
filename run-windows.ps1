# Start IndustrialRust1710 on Windows PowerShell.
param([Parameter(ValueFromRemainingArguments = $true)][string[]]$ServerArgs = @())
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location -LiteralPath $Root

$whitelist = Join-Path $Root 'whitelist.json'
if (-not (Test-Path -LiteralPath $whitelist -PathType Leaf)) {
    throw 'Missing whitelist.json. Run .\setup-windows.ps1 first and configure the whitelist.'
}
$item = Get-Item -LiteralPath $whitelist -Force
if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
    throw 'Refusing to use a reparse-point whitelist.json.'
}
$exe = Join-Path $Root 'target\release\vanilla-server.exe'
$env:Path = (Join-Path $HOME '.cargo\bin') + ';' + $env:Path
$cargo = Get-Command 'cargo.exe' -ErrorAction SilentlyContinue
if (-not $cargo) { throw 'Cargo not found. Run .\setup-windows.ps1 first.' }
Write-Host 'Checking/building vanilla-server from the current source...'
# Cargo's incremental build is a no-op when inputs are current. Always ask it
# to check so a stale release executable is never reused after source changes.
& $cargo.Source build --release -p vanilla-server --bins
if ($LASTEXITCODE -ne 0) { throw 'Rust release build failed.' }
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
    throw 'Rust release build succeeded but vanilla-server.exe is missing.'
}
Write-Host 'Starting IndustrialRust1710 (Minecraft Java 1.7.10). Press Ctrl+C to stop.'
# No server.properties: default listener is 127.0.0.1:25565.
# A normal no-argument launch now creates/loads a separate IRW4 biome world.
# Never replace, regenerate, or migrate the historical world/flat.irw here.
# Any caller-supplied arguments preserve the old explicit CLI behavior.
if ($ServerArgs.Count -eq 0) {
    $ServerArgs = @('--survival', '--expanded-seed', '42', '--world', 'world/biomes-42.irw')
    Write-Host 'Using seeded IRW4 terrain and biomes: world/biomes-42.irw (seed 42).'
    Write-Host 'Existing world/flat.irw is not modified. Pass explicit arguments to select another world.'
}
& $exe --whitelist $whitelist @ServerArgs
exit $LASTEXITCODE

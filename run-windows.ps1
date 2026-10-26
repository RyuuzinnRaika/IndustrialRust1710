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
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
    Write-Host 'Release executable missing; compiling vanilla-server...'
    $env:Path = (Join-Path $HOME '.cargo\bin') + ';' + $env:Path
    $cargo = Get-Command 'cargo.exe' -ErrorAction SilentlyContinue
    if (-not $cargo) { throw 'Cargo not found. Run .\setup-windows.ps1 first.' }
    & $cargo.Source build --release -p vanilla-server --bins
    if ($LASTEXITCODE -ne 0) { throw 'Rust release build failed.' }
}
Write-Host 'Starting IndustrialRust1710 (Minecraft Java 1.7.10). Press Ctrl+C to stop.'
# No server.properties: default listener is 127.0.0.1:25565.
& $exe --whitelist $whitelist @ServerArgs
exit $LASTEXITCODE

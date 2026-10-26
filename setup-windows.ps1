# One-time installer/build for IndustrialRust1710 on Windows 10/11 PowerShell.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location -LiteralPath $Root

if (-not (Test-Path -LiteralPath '.\Cargo.toml' -PathType Leaf) -or
    -not (Test-Path -LiteralPath '.\config\whitelist.example.json' -PathType Leaf)) {
    throw 'Run this script inside the IndustrialRust1710 repository.'
}

function Refresh-ToolPath {
    $cargoBin = Join-Path $HOME '.cargo\bin'
    $machine = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    $user = [Environment]::GetEnvironmentVariable('Path', 'User')
    $env:Path = (@($cargoBin, $machine, $user, $env:Path) | Where-Object { $_ }) -join ';'
}

function Test-MsvcTools {
    if (Get-Command 'cl.exe' -ErrorAction SilentlyContinue) { return $true }
    $programFilesX86 = [Environment]::GetEnvironmentVariable('ProgramFiles(x86)')
    if (-not $programFilesX86) { return $false }
    $vswhere = Join-Path $programFilesX86 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) { return $false }
    $found = & $vswhere -products '*' -requires 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64' -property installationPath
    $paths = @($found | Where-Object { $_ })
    return ($paths.Count -gt 0)
}

function Require-Winget {
    if (-not (Get-Command 'winget.exe' -ErrorAction SilentlyContinue)) {
        throw 'winget was not found. Install App Installer from Microsoft, or manually install Visual Studio C++ Build Tools and rustup (https://rustup.rs/).'
    }
}

Refresh-ToolPath
Write-Host '[1/4] Checking Windows MSVC C++ build tools...'
if (-not (Test-MsvcTools)) {
    Require-Winget
    Write-Host 'Installing Visual Studio 2022 Build Tools + C++ workload (may request administrator approval).'
    & winget.exe install --exact --id Microsoft.VisualStudio.2022.BuildTools --source winget `
        --accept-source-agreements --accept-package-agreements `
        --override '--wait --passive --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'
    if ($LASTEXITCODE -ne 0) { throw "Visual Studio Build Tools installation failed (exit $LASTEXITCODE)." }
    if (-not (Test-MsvcTools)) {
        throw 'MSVC C++ compiler not found after installation. Restart Windows if requested, then rerun this script.'
    }
}

Write-Host '[2/4] Installing/selecting stable Rust...'
if (-not (Get-Command 'rustup.exe' -ErrorAction SilentlyContinue)) {
    Require-Winget
    & winget.exe install --exact --id Rustlang.Rustup --source winget `
        --accept-source-agreements --accept-package-agreements
    if ($LASTEXITCODE -ne 0) { throw "rustup installation failed (exit $LASTEXITCODE)." }
    Refresh-ToolPath
}
$rustup = (Get-Command 'rustup.exe' -ErrorAction SilentlyContinue)
if (-not $rustup) { throw 'rustup.exe is unavailable after installation. Open a new PowerShell window and rerun setup.' }
& $rustup.Source toolchain install stable --profile minimal
if ($LASTEXITCODE -ne 0) { throw 'Installing stable Rust failed.' }
& $rustup.Source default stable
if ($LASTEXITCODE -ne 0) { throw 'Selecting stable Rust failed.' }
Refresh-ToolPath
$cargo = Get-Command 'cargo.exe' -ErrorAction SilentlyContinue
if (-not $cargo) { throw 'cargo.exe not found after rustup installation.' }

Write-Host '[3/4] Preparing whitelist and compiling release binaries...'
$whitelist = Join-Path $Root 'whitelist.json'
if (Test-Path -LiteralPath $whitelist) {
    $item = Get-Item -LiteralPath $whitelist -Force
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw 'Refusing to use a reparse-point whitelist.json.'
    }
    if ($item.PSIsContainer) { throw 'whitelist.json must be a file.' }
    Write-Host 'Keeping existing whitelist.json unchanged.'
} else {
    Copy-Item -LiteralPath (Join-Path $Root 'config\whitelist.example.json') -Destination $whitelist
    Write-Host 'Created whitelist.json. Replace RustTester with your Minecraft username before joining.'
}
& $cargo.Source build --release -p vanilla-server --bins
if ($LASTEXITCODE -ne 0) { throw 'Rust release build failed.' }

Write-Host '[4/4] Checking that the executable starts...'
$exe = Join-Path $Root 'target\release\vanilla-server.exe'
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) { throw "Missing release executable: $exe" }
& $exe --help | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Executable smoke test failed.' }
Write-Host "Setup finished. Edit $whitelist then run .\run-windows.ps1"
Write-Host 'Default listener: 127.0.0.1:25565 (local machine only).'

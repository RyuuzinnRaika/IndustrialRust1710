# Build verifier and run it as a separate process (avoid nested Cargo run locks).
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$VerifierArgs = @()
)
$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location -LiteralPath $Root
if ($VerifierArgs.Count -eq 0) {
    $VerifierArgs = @('--automated-only')
}
& cargo build --release -p industrial-verify
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$exe = Join-Path $Root 'target\release\industrial-verify.exe'
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
    throw "Verifier executable missing: $exe"
}
& $exe @VerifierArgs
exit $LASTEXITCODE

# Run all available automated verification checks locally (no GitHub Actions).
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
& cargo run --release -p industrial-verify -- @VerifierArgs
exit $LASTEXITCODE

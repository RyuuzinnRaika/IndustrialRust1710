# Called by run-windows.ps1 after the release binary is up to date.
# A desktop Windows PowerShell STA host is needed for the classic WinForms UI.
param(
    [Parameter(Mandatory = $true)][string]$ServerExe,
    [Parameter(Mandatory = $true)][string]$WhitelistPath,
    [string[]]$ServerArgs = @()
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ($env:OS -ne 'Windows_NT') {
    throw 'The graphical Minecraft server window is supported on Windows only.'
}
if ([System.Threading.Thread]::CurrentThread.GetApartmentState() -ne [System.Threading.ApartmentState]::STA) {
    throw 'Server GUI requires an STA PowerShell host. Run powershell.exe -STA -File .\run-windows.ps1'
}
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
$source = Join-Path $Root 'tools\ServerGui.cs'
Add-Type -Path $source -ReferencedAssemblies @('System.Windows.Forms', 'System.Drawing')
$arguments = @('--whitelist', $WhitelistPath) + @($ServerArgs)
$exitCode = [IndustrialRust1710.ServerGui]::Run($ServerExe, [string[]]$arguments, $Root)
exit $exitCode

#Requires -Version 5.1
<#
First-run installer for Windows (4.8). Run from inside an extracted
release archive that contains open-string.exe alongside this script.
Copies the binary into a per-user install directory, creates the config
directory Core writes to on first launch, and adds the install directory
to the current user's PATH if it isn't there already.
#>

$ErrorActionPreference = 'Stop'

$totalSteps = 5

function Write-Step {
    param([int]$Number, [string]$Message)
    Write-Host "[$Number/$totalSteps] $Message" -ForegroundColor Cyan
}

Write-Host "== Open String installer ==" -ForegroundColor Magenta

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$sourceExe = Join-Path $scriptDir 'open-string.exe'

Write-Step 1 "Locating open-string.exe next to this script..."
if (-not (Test-Path $sourceExe)) {
    Write-Error "open-string.exe was not found next to this script ($scriptDir). Run this installer from inside the extracted release archive."
    exit 1
}
Write-Host "  found: $sourceExe" -ForegroundColor Green

$installDir = Join-Path $env:LOCALAPPDATA 'OpenString\bin'
$configDir = Join-Path $env:APPDATA 'open-string'

Write-Step 2 "Creating install and config directories..."
New-Item -ItemType Directory -Force -Path $installDir | Out-Null
New-Item -ItemType Directory -Force -Path $configDir | Out-Null
Write-Host "  install dir: $installDir" -ForegroundColor Green
Write-Host "  config dir:  $configDir" -ForegroundColor Green

Write-Step 3 "Copying binary..."
Copy-Item -Path $sourceExe -Destination (Join-Path $installDir 'open-string.exe') -Force
Write-Host "  copied to $installDir\open-string.exe" -ForegroundColor Green

Write-Step 4 "Updating user PATH..."
$userPath = [Environment]::GetEnvironmentVariable('PATH', 'User')
$pathEntries = @()
if ($userPath) {
    $pathEntries = $userPath.Split(';') | Where-Object { $_ -ne '' }
}

if ($pathEntries -notcontains $installDir) {
    $newPath = if ($userPath) { "$userPath;$installDir" } else { $installDir }
    [Environment]::SetEnvironmentVariable('PATH', $newPath, 'User')
    Write-Host "  added $installDir to your user PATH (open a new terminal for it to take effect)" -ForegroundColor Green
} else {
    Write-Host "  $installDir is already on your user PATH" -ForegroundColor Yellow
}

Write-Step 5 "Verifying installation..."
$installedExe = Join-Path $installDir 'open-string.exe'
$installedVersion = $null
try {
    $installedVersion = & $installedExe --version 2>$null
} catch {
    $installedVersion = $null
}
if ($installedVersion) {
    Write-Host "  $installedVersion" -ForegroundColor Green
} else {
    Write-Host "  warning: could not run $installedExe --version to confirm the install" -ForegroundColor Yellow
}

Write-Host ""
Write-Host "== Installation complete ==" -ForegroundColor Magenta
Write-Host "  binary:      $installDir\open-string.exe"
Write-Host "  config/logs: $configDir"
Write-Host "  next step:   open a new terminal and run 'open-string auth login'"

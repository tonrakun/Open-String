#Requires -Version 5.1
<#
First-run installer for Windows (4.8). Run from inside an extracted
release archive that contains open-string.exe alongside this script.
Copies the binary into a per-user install directory, creates the config
directory Core writes to on first launch, and adds the install directory
to the current user's PATH if it isn't there already.
#>

$ErrorActionPreference = 'Stop'

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$sourceExe = Join-Path $scriptDir 'open-string.exe'

if (-not (Test-Path $sourceExe)) {
    Write-Error "open-string.exe was not found next to this script ($scriptDir). Run this installer from inside the extracted release archive."
    exit 1
}

$installDir = Join-Path $env:LOCALAPPDATA 'OpenString\bin'
$configDir = Join-Path $env:APPDATA 'open-string'

New-Item -ItemType Directory -Force -Path $installDir | Out-Null
New-Item -ItemType Directory -Force -Path $configDir | Out-Null

Copy-Item -Path $sourceExe -Destination (Join-Path $installDir 'open-string.exe') -Force

$userPath = [Environment]::GetEnvironmentVariable('PATH', 'User')
$pathEntries = @()
if ($userPath) {
    $pathEntries = $userPath.Split(';') | Where-Object { $_ -ne '' }
}

if ($pathEntries -notcontains $installDir) {
    $newPath = if ($userPath) { "$userPath;$installDir" } else { $installDir }
    [Environment]::SetEnvironmentVariable('PATH', $newPath, 'User')
    Write-Host "Added $installDir to your user PATH. Open a new terminal for it to take effect."
} else {
    Write-Host "$installDir is already on your user PATH."
}

Write-Host "Open String installed to $installDir"
Write-Host "Config/audit log directory: $configDir"
Write-Host "Run 'open-string auth login' in a new terminal to get started."

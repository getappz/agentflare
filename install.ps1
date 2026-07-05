<#
install.ps1 - Build leanstack locally on Windows and install it into Cargo's bin directory.

Building locally (rather than downloading a prebuilt .exe) means the binary
compiled on your own machine, so there's no unsigned-binary AV heuristic to
trip — the same reason engram's own install docs steer Windows users to
`go install` over their prebuilt release.

Usage:
    .\install.ps1
    .\install.ps1 -BuildOnly
#>

param(
    [switch]$BuildOnly,
    [switch]$Help
)

$ErrorActionPreference = 'Stop'

if ($Help) {
    Write-Host 'Usage: .\install.ps1 [-BuildOnly] [-Help]'
    Write-Host ''
    Write-Host '  (no args)    Build leanstack locally and install it into Cargo''s bin directory'
    Write-Host '  -BuildOnly   Build only, do not install'
    Write-Host '  -Help        Show this help message'
    exit 0
}

function Get-CargoBinDir {
    if ($env:CARGO_HOME) {
        return Join-Path $env:CARGO_HOME 'bin'
    }
    return Join-Path $HOME '.cargo\bin'
}

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path

if (-not (Test-Path (Join-Path $scriptDir 'Cargo.toml') -PathType Leaf)) {
    throw "Cargo.toml not found next to this script — run install.ps1 from a leanstack checkout, or clone https://github.com/getappz/leanstack first."
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw 'cargo not found. Install Rust from https://rustup.rs/'
}

$cargoBinDir = Get-CargoBinDir
$builtBinary = Join-Path $scriptDir 'target\release\leanstack.exe'
$installedBinary = Join-Path $cargoBinDir 'leanstack.exe'

Write-Host 'leanstack Windows installer'
Write-Host 'Mode: build from source'
Write-Host ''
Write-Host 'Building leanstack (release)...'

Push-Location $scriptDir
try {
    & cargo build --release
}
finally {
    Pop-Location
}

if (-not (Test-Path $builtBinary -PathType Leaf)) {
    throw "Build failed - binary not found at $builtBinary"
}

Write-Host "Built: $builtBinary"

if ($BuildOnly) {
    Write-Host 'Done (build only).'
    exit 0
}

New-Item -ItemType Directory -Path $cargoBinDir -Force | Out-Null

$tempBinary = Join-Path $cargoBinDir ('.leanstack.new.' + $PID + '.exe')
Copy-Item -Path $builtBinary -Destination $tempBinary -Force
Move-Item -Path $tempBinary -Destination $installedBinary -Force

Write-Host "Installed: $installedBinary"

$pathEntries = @($env:Path -split ';' | Where-Object { $_ })
if ($pathEntries -notcontains $cargoBinDir) {
    Write-Host ''
    Write-Warning "$cargoBinDir is not in your PATH."
    Write-Host 'Add it to your user PATH, then restart your shell.'
}

Write-Host ''
Write-Host 'Done! Verify with: leanstack --version'
Write-Host 'Next step: leanstack init --agent <claude-code|codex|cursor|windsurf|vscode-copilot|cline|continue>'

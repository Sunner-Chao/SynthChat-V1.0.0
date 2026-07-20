param(
  [ValidateSet("all", "nsis", "msi")]
  [string]$Bundle = "nsis",
  [string]$RustToolchain,
  [switch]$SkipNpmInstall,
  [switch]$PreflightOnly,
  [switch]$OpenOutput
)

$ErrorActionPreference = "Stop"
$Workspace = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$BuildScript = Join-Path $Workspace "build-one-click.ps1"
$Arguments = @{ Bundle = $Bundle }
if (-not [string]::IsNullOrWhiteSpace($RustToolchain)) {
  $Arguments.RustToolchain = $RustToolchain
}
if ($SkipNpmInstall) { $Arguments.SkipNpmInstall = $true }
if ($PreflightOnly) { $Arguments.PreflightOnly = $true }
if ($OpenOutput) { $Arguments.OpenOutput = $true }

& $BuildScript @Arguments
if ($LASTEXITCODE -ne 0) {
  exit $LASTEXITCODE
}

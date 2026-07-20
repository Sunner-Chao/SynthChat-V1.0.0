param(
  [ValidateSet("all", "nsis", "msi")]
  [string]$Bundle = "nsis",
  [string]$RustToolchain,
  [switch]$SkipNpmInstall,
  [switch]$PreflightOnly,
  [switch]$OpenOutput
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$Workspace = (Resolve-Path $PSScriptRoot).Path
Set-Location $Workspace
$Frontend = Join-Path $Workspace "frontend"
$Desktop = Join-Path $Workspace "desktop"
$RunTauri = Join-Path $Workspace "scripts\run-tauri.mjs"
$BundleRoot = Join-Path $Desktop "target\release\bundle"
$NodeVersion = "22.14.0"
$NpmVersion = "10.9.2"

function Assert-Command {
  param([Parameter(Mandatory = $true)][string]$Name)
  if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
    throw "Required command not found: $Name"
  }
}

Assert-Command "cargo"
Assert-Command "npx"
Assert-Command "rustc"

$ResolvedRustToolchain = $RustToolchain
if ([string]::IsNullOrWhiteSpace($ResolvedRustToolchain)) {
  $ResolvedRustToolchain = $env:SYNTHCHAT_RUST_TOOLCHAIN
}
if ([string]::IsNullOrWhiteSpace($ResolvedRustToolchain)) {
  $ResolvedRustToolchain = if ([Environment]::Is64BitOperatingSystem) {
    "1.88.0-x86_64-pc-windows-msvc"
  }
  else {
    throw "The Windows one-click build requires an explicit supported Rust toolchain on non-x64 hosts."
  }
}
$ResolvedRustToolchain = $ResolvedRustToolchain.TrimStart("+")
if ($ResolvedRustToolchain -notmatch '\A[A-Za-z0-9][A-Za-z0-9._-]{0,127}\z') {
  throw "Rust toolchain override contains unsupported characters."
}
$env:RUSTUP_TOOLCHAIN = $ResolvedRustToolchain
$env:SYNTHCHAT_RUST_TOOLCHAIN = $ResolvedRustToolchain
$LockedRuntime = @("--yes", "-p", "node@$NodeVersion", "-p", "npm@$NpmVersion")

& npx @LockedRuntime node --version
if ($LASTEXITCODE -ne 0) { throw "Locked Node runtime is unavailable." }
& npx @LockedRuntime npm --version
if ($LASTEXITCODE -ne 0) { throw "Locked npm runtime is unavailable." }

if (-not $SkipNpmInstall) {
  & npx @LockedRuntime npm ci --prefix $Frontend
  if ($LASTEXITCODE -ne 0) {
    throw "Frontend dependency installation failed with exit code $LASTEXITCODE."
  }
}
if (-not (Test-Path -LiteralPath $RunTauri -PathType Leaf)) {
  throw "Tauri launcher is missing: $RunTauri"
}

if ($PreflightOnly) {
  & npx @LockedRuntime npm run build
  if ($LASTEXITCODE -ne 0) { throw "Frontend build failed." }
  & cargo "+$ResolvedRustToolchain" check --locked --manifest-path (Join-Path $Workspace "backend\Cargo.toml") --all-targets
  if ($LASTEXITCODE -ne 0) { throw "Backend check failed." }
  & cargo "+$ResolvedRustToolchain" check --locked --manifest-path (Join-Path $Workspace "desktop\Cargo.toml") --all-targets
  if ($LASTEXITCODE -ne 0) { throw "Desktop check failed." }
  Write-Host "Preflight complete." -ForegroundColor Green
  exit 0
}

$Arguments = @("build")
if ($Bundle -ne "all") {
  $Arguments += @("--bundles", $Bundle)
}
$Arguments += @("--", "--locked")

Push-Location $Desktop
try {
  & npx @LockedRuntime node $RunTauri @Arguments
  if ($LASTEXITCODE -ne 0) {
    throw "Tauri build failed with exit code $LASTEXITCODE."
  }
}
finally {
  Pop-Location
}

if (-not (Test-Path -LiteralPath $BundleRoot -PathType Container)) {
  throw "Tauri completed without a bundle directory: $BundleRoot"
}

Write-Host "Desktop bundle ready: $BundleRoot" -ForegroundColor Green
if ($OpenOutput) {
  Invoke-Item $BundleRoot
}

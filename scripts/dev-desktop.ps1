[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$workspace = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$backendManifest = Join-Path $workspace "backend\Cargo.toml"
$desktopDirectory = Join-Path $workspace "desktop"
$tauriCli = Join-Path $workspace "frontend\node_modules\.bin\tauri.cmd"

$frontendHost = if ([string]::IsNullOrWhiteSpace($env:SYNTHCHAT_FRONTEND_HOST)) {
    "127.0.0.1"
}
else {
    $env:SYNTHCHAT_FRONTEND_HOST.Trim()
}
if ($frontendHost -notin @("127.0.0.1", "localhost", "::1")) {
    throw "SYNTHCHAT_FRONTEND_HOST must use a loopback hostname."
}

$frontendPortText = if ([string]::IsNullOrWhiteSpace($env:SYNTHCHAT_FRONTEND_PORT)) {
    "1421"
}
else {
    $env:SYNTHCHAT_FRONTEND_PORT.Trim()
}
$frontendPort = 0
if (
    -not [int]::TryParse($frontendPortText, [ref]$frontendPort) -or
    $frontendPort -lt 1 -or
    $frontendPort -gt 65535
) {
    throw "SYNTHCHAT_FRONTEND_PORT must be an integer between 1 and 65535."
}
$frontendAuthority = if ($frontendHost.Contains(":")) {
    "[$frontendHost]:$frontendPort"
}
else {
    "${frontendHost}:$frontendPort"
}
$frontendOrigin = "http://$frontendAuthority"

if (-not $backendManifest.StartsWith($workspace, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Backend manifest escaped the workspace."
}

Push-Location $workspace
$previousBackendBinary = $env:SYNTHCHAT_BACKEND_BINARY
$previousFrontendHost = $env:SYNTHCHAT_FRONTEND_HOST
$previousFrontendPort = $env:SYNTHCHAT_FRONTEND_PORT
$previousDesktopDevOrigin = $env:SYNTHCHAT_DESKTOP_DEV_ORIGIN
$tauriConfigPath = $null
try {
    $buildMessages = @(cargo build --manifest-path $backendManifest --message-format json-render-diagnostics)
    if ($LASTEXITCODE -ne 0) {
        throw "Backend build failed with exit code $LASTEXITCODE."
    }
    $backendBinary = $null
    foreach ($line in $buildMessages) {
        try {
            $message = $line | ConvertFrom-Json -ErrorAction Stop
        }
        catch {
            continue
        }
        if (
            $message.reason -eq "compiler-artifact" -and
            $message.target.name -eq "synthchat-hermes-backend" -and
            $message.target.kind -contains "bin" -and
            -not [string]::IsNullOrWhiteSpace($message.executable)
        ) {
            $backendBinary = $message.executable
        }
    }
    if ([string]::IsNullOrWhiteSpace($backendBinary)) {
        throw "Backend build did not report the executable artifact."
    }
    if (-not (Test-Path -LiteralPath $backendBinary -PathType Leaf)) {
        throw "Backend build completed without producing the expected executable."
    }
    if (-not (Test-Path -LiteralPath $tauriCli -PathType Leaf)) {
        throw "Tauri CLI is missing. Run npm ci --prefix frontend first."
    }

    $env:SYNTHCHAT_BACKEND_BINARY = (Resolve-Path $backendBinary).Path
    $env:SYNTHCHAT_FRONTEND_HOST = $frontendHost
    $env:SYNTHCHAT_FRONTEND_PORT = $frontendPortText
    $env:SYNTHCHAT_DESKTOP_DEV_ORIGIN = $frontendOrigin
    $tauriConfigOverride = @{
        build = @{
            devUrl = $frontendOrigin
        }
    } | ConvertTo-Json -Compress
    $tauriConfigPath = Join-Path (
        [System.IO.Path]::GetTempPath()
    ) "synthchat-tauri-dev-$([guid]::NewGuid().ToString('N')).json"
    $utf8WithoutBom = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($tauriConfigPath, $tauriConfigOverride, $utf8WithoutBom)
    Push-Location $desktopDirectory
    try {
        # Passing inline JSON through a Windows .cmd shim strips its quotes.
        & $tauriCli dev --config $tauriConfigPath
        if ($LASTEXITCODE -ne 0) {
            throw "Tauri development process failed with exit code $LASTEXITCODE."
        }
    }
    finally {
        Pop-Location
    }
}
finally {
    $env:SYNTHCHAT_BACKEND_BINARY = $previousBackendBinary
    $env:SYNTHCHAT_FRONTEND_HOST = $previousFrontendHost
    $env:SYNTHCHAT_FRONTEND_PORT = $previousFrontendPort
    $env:SYNTHCHAT_DESKTOP_DEV_ORIGIN = $previousDesktopDevOrigin
    Pop-Location
    if (
        -not [string]::IsNullOrWhiteSpace($tauriConfigPath) -and
        (Test-Path -LiteralPath $tauriConfigPath -PathType Leaf)
    ) {
        Remove-Item -LiteralPath $tauriConfigPath -Force -ErrorAction SilentlyContinue
    }
}

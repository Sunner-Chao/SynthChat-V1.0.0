[CmdletBinding()]
param(
  [string]$BackendBinary,
  [string]$CargoExecutable,
  [string]$RustToolchain,
  [ValidateRange(1, 86400)]
  [int]$DurationSeconds = 15,
  [ValidateRange(1, 64)]
  [int]$Concurrency = 4,
  [ValidateRange(1, 60)]
  [int]$SampleIntervalSeconds = 1,
  [ValidateRange(1, 120)]
  [int]$StartupTimeoutSeconds = 30,
  [ValidateRange(1, 120)]
  [int]$RequestTimeoutSeconds = 10,
  [ValidateRange(1, 3600)]
  [int]$CargoTimeoutSeconds = 300,
  [ValidateRange(10, 2000)]
  [int]$StartupPollMilliseconds = 50,
  [ValidateRange(10, 2000)]
  [int]$WorkerPollMilliseconds = 100,
  [ValidateRange(64, 4096)]
  [int]$StartupHandshakeMaxBytes = 128,
  [ValidateRange(1, 120)]
  [int]$ShutdownGraceSeconds = 5,
  [ValidateRange(1, 120)]
  [int]$KillTimeoutSeconds = 5,
  [ValidateRange(1, 30)]
  [int]$FaultProbeTimeoutSeconds = 2,
  [ValidateRange(1, 20)]
  [int]$FaultProbeAttempts = 3,
  [ValidateRange(10, 5000)]
  [int]$FaultProbeDelayMilliseconds = 100,
  [ValidateRange(128, 16384)]
  [int]$LatencySampleLimit = 2048,
  [switch]$SkipBuild,
  [switch]$IncludeFaultChecks,
  [string]$ResultPath,
  [switch]$KeepHermesHome
)

<#
.SYNOPSIS
Runs a local, credential-safe runtime check for the standalone Rust backend.

.DESCRIPTION
The script asks the backend to bind an OS-assigned IPv4 loopback port with a temporary
HERMES_HOME. It exercises public health, authenticated read-only API requests,
CORS/authentication boundaries, concurrent health/capabilities traffic, and an
optional managed crash/restart probe. It samples the backend process working
set, private bytes, and Windows handle count while traffic is in flight.

Each generated desktop token is passed only through the child stdin pipe and
in-memory HTTP request headers. The child environment explicitly removes any
inherited token. Tokens are never placed in process arguments or emitted to
stdout. Before a result is returned or written, the script verifies that its
serialized form contains none of the tokens used by any backend generation.

.EXAMPLE
./scripts/verify-backend-runtime.ps1 -DurationSeconds 15 -Concurrency 4 `
  -IncludeFaultChecks -ResultPath ./logs/phase4/runtime-short.json

.EXAMPLE
./scripts/verify-backend-runtime.ps1 -DurationSeconds 28800 -Concurrency 8 `
  -SampleIntervalSeconds 30 -IncludeFaultChecks `
  -ResultPath ./logs/phase4/runtime-soak-8h.json
#>

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Add-HttpAssembly {
  try {
    Add-Type -AssemblyName System.Net.Http -ErrorAction Stop
  }
  catch {
    # PowerShell 7 already loads this assembly. A duplicate load is harmless.
    if (-not ("System.Net.Http.HttpClient" -as [type])) {
      throw
    }
  }
}

function Get-WorkspaceRoot {
  return (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
}

function New-DesktopToken {
  $bytes = New-Object byte[] 48
  [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
  # 48 bytes becomes a 64-byte, visible-ASCII Base64 value with no padding.
  return [Convert]::ToBase64String($bytes)
}

function Protect-Text {
  param(
    [AllowNull()]
    [string]$Value,
    [AllowEmptyCollection()]
    [string[]]$Secrets
  )

  if ($null -eq $Value) {
    return $null
  }
  $protected = $Value
  foreach ($secret in @($Secrets)) {
    if (-not [string]::IsNullOrEmpty($secret)) {
      $protected = $protected.Replace($secret, "[REDACTED]")
    }
  }
  return $protected
}

function Test-IsWindows {
  return [System.Environment]::OSVersion.Platform -eq [System.PlatformID]::Win32NT
}

function Resolve-CargoExecutable {
  param([string]$RequestedExecutable)

  $candidate = $RequestedExecutable
  if ([string]::IsNullOrWhiteSpace($candidate)) {
    $candidate = [System.Environment]::GetEnvironmentVariable("SYNTHCHAT_VERIFY_CARGO")
  }
  if ([string]::IsNullOrWhiteSpace($candidate)) {
    $candidate = [System.Environment]::GetEnvironmentVariable("CARGO")
  }
  if ([string]::IsNullOrWhiteSpace($candidate)) {
    $candidate = "cargo"
  }

  $command = Get-Command -Name $candidate -CommandType Application -ErrorAction Stop | Select-Object -First 1
  return $command.Source
}

function Resolve-RustToolchain {
  param([string]$RequestedToolchain)

  $resolved = $RequestedToolchain
  if ([string]::IsNullOrWhiteSpace($resolved)) {
    $resolved = [System.Environment]::GetEnvironmentVariable("SYNTHCHAT_VERIFY_RUST_TOOLCHAIN")
  }
  if ([string]::IsNullOrWhiteSpace($resolved)) {
    return $null
  }
  $resolved = $resolved.Trim()
  if ($resolved.StartsWith("+")) {
    $resolved = $resolved.Substring(1)
  }
  if ($resolved -notmatch '\A[A-Za-z0-9][A-Za-z0-9._-]{0,127}\z') {
    throw "Rust toolchain override contains unsupported characters."
  }
  return $resolved
}

function ConvertTo-NativeArgumentString {
  param([string[]]$Arguments)

  $quoted = foreach ($argument in $Arguments) {
    if ($argument -notmatch '[\s"]') {
      $argument
      continue
    }

    $builder = [System.Text.StringBuilder]::new()
    [void]$builder.Append('"')
    $slashes = 0
    foreach ($character in $argument.ToCharArray()) {
      if ($character -eq '\') {
        $slashes++
        continue
      }
      if ($character -eq '"') {
        [void]$builder.Append(('\' * (($slashes * 2) + 1)))
        [void]$builder.Append('"')
      }
      else {
        [void]$builder.Append(('\' * $slashes))
        [void]$builder.Append($character)
      }
      $slashes = 0
    }
    [void]$builder.Append(('\' * ($slashes * 2)))
    [void]$builder.Append('"')
    $builder.ToString()
  }
  return [string]::Join(' ', [string[]]$quoted)
}

function Invoke-CargoCommand {
  param(
    [string]$Executable,
    [AllowNull()]
    [string]$Toolchain,
    [string[]]$Arguments,
    [string]$WorkingDirectory,
    [int]$TimeoutSeconds,
    [int]$KillTimeoutSeconds
  )

  $cargoArguments = New-Object System.Collections.Generic.List[string]
  if (-not [string]::IsNullOrWhiteSpace($Toolchain)) {
    $cargoArguments.Add("+$Toolchain")
  }
  foreach ($argument in $Arguments) {
    $cargoArguments.Add($argument)
  }

  $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
  $startInfo.FileName = $Executable
  $startInfo.WorkingDirectory = $WorkingDirectory
  $startInfo.UseShellExecute = $false
  $startInfo.CreateNoWindow = $true
  $startInfo.RedirectStandardOutput = $true
  $startInfo.RedirectStandardError = $true
  [void]$startInfo.EnvironmentVariables.Remove("SYNTHCHAT_DESKTOP_TOKEN")
  if ($null -ne $startInfo.PSObject.Properties["ArgumentList"]) {
    foreach ($argument in $cargoArguments) {
      $startInfo.ArgumentList.Add($argument)
    }
  }
  else {
    $startInfo.Arguments = ConvertTo-NativeArgumentString -Arguments $cargoArguments.ToArray()
  }

  $process = [System.Diagnostics.Process]::new()
  $process.StartInfo = $startInfo
  try {
    if (-not $process.Start()) {
      throw "Cargo could not be started."
    }
    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()
    if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
      try { $process.Kill($true) } catch { try { $process.Kill() } catch {} }
      if (-not $process.WaitForExit($KillTimeoutSeconds * 1000)) {
        throw "Cargo exceeded its timeout and did not exit within the configured kill timeout."
      }
      throw "Cargo exceeded the configured $TimeoutSeconds second timeout."
    }
    $stdout = $stdoutTask.GetAwaiter().GetResult()
    $stderr = $stderrTask.GetAwaiter().GetResult()
    if ($process.ExitCode -ne 0) {
      throw "Cargo exited with code $($process.ExitCode): $stderr"
    }
    return $stdout
  }
  finally {
    $process.Dispose()
  }
}

function Resolve-BackendBinary {
  param(
    [string]$Workspace,
    [string]$RequestedBinary,
    [bool]$SkipCompile,
    [string]$Cargo,
    [AllowNull()]
    [string]$Toolchain,
    [int]$CargoTimeout,
    [int]$ProcessKillTimeout
  )

  $manifest = Join-Path $Workspace "backend\Cargo.toml"
  if (-not (Test-Path -LiteralPath $manifest -PathType Leaf)) {
    throw "Backend Cargo manifest was not found: $manifest"
  }

  $metadataJson = Invoke-CargoCommand -Executable $Cargo -Toolchain $Toolchain `
    -Arguments @("metadata", "--format-version", "1", "--no-deps", "--manifest-path", $manifest) `
    -WorkingDirectory $Workspace -TimeoutSeconds $CargoTimeout `
    -KillTimeoutSeconds $ProcessKillTimeout
  try {
    $metadata = $metadataJson | ConvertFrom-Json
  }
  catch {
    throw "Cargo metadata returned invalid JSON: $($_.Exception.Message)"
  }
  if ([string]::IsNullOrWhiteSpace([string]$metadata.target_directory)) {
    throw "Cargo metadata did not provide target_directory."
  }

  $manifestFullPath = [System.IO.Path]::GetFullPath($manifest)
  $package = @($metadata.packages | Where-Object {
      [System.IO.Path]::GetFullPath([string]$_.manifest_path) -eq $manifestFullPath
    }) | Select-Object -First 1
  if ($null -eq $package) {
    throw "Cargo metadata did not include the backend package."
  }
  $binaryTarget = @($package.targets | Where-Object {
      @($_.kind) -contains "bin" -and $_.name -eq $package.name
    }) | Select-Object -First 1
  if ($null -eq $binaryTarget) {
    $binaryTarget = @($package.targets | Where-Object { @($_.kind) -contains "bin" }) | Select-Object -First 1
  }
  if ($null -eq $binaryTarget) {
    throw "Cargo metadata did not expose a backend binary target."
  }

  if (-not $SkipCompile) {
    [void](Invoke-CargoCommand -Executable $Cargo -Toolchain $Toolchain `
        -Arguments @("build", "--manifest-path", $manifest, "--bin", [string]$binaryTarget.name) `
        -WorkingDirectory $Workspace -TimeoutSeconds $CargoTimeout `
        -KillTimeoutSeconds $ProcessKillTimeout)
  }

  if ([string]::IsNullOrWhiteSpace($RequestedBinary)) {
    $executableName = [string]$binaryTarget.name
    if (Test-IsWindows) {
      $executableName += ".exe"
    }
    $requested = Join-Path (Join-Path ([string]$metadata.target_directory) "debug") $executableName
  }
  elseif ([System.IO.Path]::IsPathRooted($RequestedBinary)) {
    $requested = $RequestedBinary
  }
  else {
    $requested = Join-Path $Workspace $RequestedBinary
  }

  if (-not (Test-Path -LiteralPath $requested -PathType Leaf)) {
    throw "Backend executable was not found: $requested. Re-run without -SkipBuild or set -BackendBinary."
  }
  return (Resolve-Path -LiteralPath $requested).Path
}

function New-HttpClient {
  param([int]$TimeoutSeconds)

  $client = [System.Net.Http.HttpClient]::new()
  $client.Timeout = [TimeSpan]::FromSeconds($TimeoutSeconds)
  return $client
}

function Invoke-BackendRequest {
  param(
    [System.Net.Http.HttpClient]$Client,
    [string]$Method,
    [string]$Uri,
    [hashtable]$Headers
  )

  $request = [System.Net.Http.HttpRequestMessage]::new(
    [System.Net.Http.HttpMethod]::new($Method),
    $Uri
  )
  try {
    foreach ($entry in $Headers.GetEnumerator()) {
      [void]$request.Headers.TryAddWithoutValidation($entry.Key, [string]$entry.Value)
    }

    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    $response = $Client.SendAsync($request).GetAwaiter().GetResult()
    try {
      $body = $response.Content.ReadAsStringAsync().GetAwaiter().GetResult()
      $headers = @{}
      foreach ($header in $response.Headers) {
        $headers[$header.Key] = [string]::Join(",", $header.Value)
      }
      foreach ($header in $response.Content.Headers) {
        $headers[$header.Key] = [string]::Join(",", $header.Value)
      }
      return [pscustomobject]@{
        StatusCode = [int]$response.StatusCode
        Headers = $headers
        Body = $body
        LatencyMs = [Math]::Round($stopwatch.Elapsed.TotalMilliseconds, 3)
      }
    }
    finally {
      $response.Dispose()
    }
  }
  finally {
    $request.Dispose()
  }
}

function Start-Backend {
  param(
    [string]$Binary,
    [string]$HermesHome,
    [string]$DesktopToken,
    [string]$WorkingDirectory,
    [int]$HandshakeTimeoutSeconds,
    [int]$HandshakeMaxBytes,
    [int]$PollMilliseconds,
    [int]$ShutdownGrace,
    [int]$KillTimeout
  )

  $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
  $startInfo.FileName = $Binary
  $startInfo.WorkingDirectory = $WorkingDirectory
  $startInfo.UseShellExecute = $false
  $startInfo.CreateNoWindow = $true
  $startInfo.RedirectStandardInput = $true
  $startInfo.RedirectStandardOutput = $true
  $startInfo.RedirectStandardError = $true
  $startInfo.EnvironmentVariables["SYNTHCHAT_BACKEND_ADDR"] = "127.0.0.1:0"
  [void]$startInfo.EnvironmentVariables.Remove("SYNTHCHAT_DESKTOP_TOKEN")
  $startInfo.EnvironmentVariables["HERMES_HOME"] = $HermesHome

  $process = [System.Diagnostics.Process]::new()
  $process.StartInfo = $startInfo
  $server = [pscustomobject]@{
    Process = $process
    Port = $null
    BaseUri = $null
    StandardInput = $null
    StdoutTask = $null
    StderrTask = $null
  }
  try {
    if (-not $process.Start()) {
      throw "The backend process could not be started."
    }
    $server.StandardInput = $process.StandardInput
    $server.StderrTask = $process.StandardError.ReadToEndAsync()

    $server.StandardInput.WriteLine($DesktopToken)
    $server.StandardInput.Flush()
    $address = Read-StartupHandshake -Reader $process.StandardOutput -Process $process `
      -TimeoutSeconds $HandshakeTimeoutSeconds -MaxBytes $HandshakeMaxBytes `
      -PollMilliseconds $PollMilliseconds
    $server.Port = $address.Port
    $server.BaseUri = "http://127.0.0.1:$($address.Port)"
    $server.StdoutTask = $process.StandardOutput.ReadToEndAsync()
    return $server
  }
  catch {
    $message = Protect-Text -Value $_.Exception.Message -Secrets @($DesktopToken)
    try {
      Stop-Backend -Server $server -ShutdownGraceSeconds $ShutdownGrace -KillTimeoutSeconds $KillTimeout
    }
    catch {
      $stopMessage = Protect-Text -Value $_.Exception.Message -Secrets @($DesktopToken)
      throw "$message Backend cleanup also failed: $stopMessage"
    }
    throw $message
  }
}

function Read-StartupHandshake {
  param(
    [System.IO.StreamReader]$Reader,
    [System.Diagnostics.Process]$Process,
    [int]$TimeoutSeconds,
    [int]$MaxBytes,
    [int]$PollMilliseconds
  )

  $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
  $builder = [System.Text.StringBuilder]::new()
  $buffer = New-Object char[] 1
  $bytesRead = 0

  while ($true) {
    if ([DateTime]::UtcNow -ge $deadline) {
      throw "Backend startup handshake timed out after $TimeoutSeconds seconds."
    }
    $readTask = $Reader.ReadAsync($buffer, 0, 1)
    while (-not $readTask.IsCompleted) {
      $remaining = [int][Math]::Ceiling(($deadline - [DateTime]::UtcNow).TotalMilliseconds)
      if ($remaining -le 0) {
        throw "Backend startup handshake timed out after $TimeoutSeconds seconds."
      }
      [void]$readTask.Wait([Math]::Min($PollMilliseconds, $remaining))
    }

    $readCount = $readTask.GetAwaiter().GetResult()
    if ($readCount -eq 0) {
      if ($Process.HasExited) {
        throw "Backend exited before emitting its startup handshake."
      }
      throw "Backend closed stdout before emitting its startup handshake."
    }

    $bytesRead++
    if ($bytesRead -gt $MaxBytes) {
      throw "Backend startup handshake exceeded the configured $MaxBytes byte limit."
    }
    $character = $buffer[0]
    if ($character -eq "`n") {
      break
    }
    $codePoint = [int][char]$character
    if ($character -ne "`r" -and ($codePoint -lt 0x20 -or $codePoint -gt 0x7e)) {
      throw "Backend startup handshake contained non-ASCII or control characters."
    }
    [void]$builder.Append($character)
  }

  $line = $builder.ToString()
  if ($line.EndsWith("`r")) {
    $line = $line.Substring(0, $line.Length - 1)
  }
  if ($line -notmatch '\ASYNTHCHAT_BACKEND_READY 127\.0\.0\.1:([1-9][0-9]{0,4})\z') {
    throw "Backend startup handshake did not match the required loopback format."
  }
  $port = [int]$Matches[1]
  if ($port -lt 1 -or $port -gt 65535) {
    throw "Backend startup handshake reported an invalid port."
  }
  return [System.Net.IPEndPoint]::new([System.Net.IPAddress]::Loopback, $port)
}

function Get-BackendDiagnostics {
  param(
    [AllowNull()]$Server,
    [AllowEmptyCollection()]
    [string[]]$DesktopTokens
  )

  if ($null -eq $Server) {
    return $null
  }
  $process = $Server.Process
  try {
    if (-not $process.HasExited) {
      return [pscustomobject]@{ State = "running" }
    }
  }
  catch {
    return [pscustomobject]@{ State = "disposed" }
  }

  $stdout = ""
  $stderr = ""
  try { $stdout = $Server.StdoutTask.GetAwaiter().GetResult() } catch {}
  try { $stderr = $Server.StderrTask.GetAwaiter().GetResult() } catch {}
  return [pscustomobject]@{
    State = "exited"
    ExitCode = $process.ExitCode
    Stdout = Protect-Text -Value $stdout -Secrets $DesktopTokens
    Stderr = Protect-Text -Value $stderr -Secrets $DesktopTokens
  }
}

function Stop-Backend {
  param(
    [AllowNull()]$Server,
    [int]$ShutdownGraceSeconds,
    [int]$KillTimeoutSeconds
  )

  if ($null -eq $Server) {
    return
  }
  $process = $Server.Process
  try {
    if ($null -ne $Server.StandardInput) {
      try { $Server.StandardInput.Close() } catch {}
      try { $Server.StandardInput.Dispose() } catch {}
      $Server.StandardInput = $null
    }
    try { $hasExited = $process.HasExited } catch { return }
    if (-not $hasExited) {
      if ($process.WaitForExit($ShutdownGraceSeconds * 1000)) {
        return
      }
      try { $process.Kill($true) } catch { $process.Kill() }
      if (-not $process.WaitForExit($KillTimeoutSeconds * 1000)) {
        throw "Backend did not exit after stdin closure and the configured kill timeout."
      }
    }
  }
  finally {
    try { $process.Dispose() } catch {}
  }
}

function Wait-ForBackendHealth {
  param(
    $Server,
    [System.Net.Http.HttpClient]$Client,
    [int]$TimeoutSeconds,
    [int]$PollMilliseconds,
    [AllowEmptyCollection()]
    [string[]]$DesktopTokens
  )

  $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
  $lastError = $null
  while ([DateTime]::UtcNow -lt $deadline) {
    if ($Server.Process.HasExited) {
      $diagnostics = Get-BackendDiagnostics -Server $Server -DesktopTokens $DesktopTokens
      throw "Backend exited during startup: $($diagnostics | ConvertTo-Json -Compress -Depth 4)"
    }
    try {
      $response = Invoke-BackendRequest -Client $Client -Method "GET" -Uri "$($Server.BaseUri)/health" -Headers @{}
      if ($response.StatusCode -eq 200) {
        return $response
      }
      $lastError = "health returned HTTP $($response.StatusCode)"
    }
    catch {
      $lastError = Protect-Text -Value $_.Exception.Message -Secrets $DesktopTokens
    }
    Start-Sleep -Milliseconds $PollMilliseconds
  }
  throw "Backend did not become healthy within $TimeoutSeconds seconds: $lastError"
}

function Get-ProcessSample {
  param([System.Diagnostics.Process]$Process)

  try {
    if ($Process.HasExited) {
      return [pscustomobject]@{
        TimestampUtc = [DateTime]::UtcNow.ToString("o")
        State = "exited"
        WorkingSetBytes = $null
        PrivateMemoryBytes = $null
        HandleCount = $null
      }
    }
    $Process.Refresh()
    $handleCount = $null
    try { $handleCount = [int64]$Process.HandleCount } catch {}
    return [pscustomobject]@{
      TimestampUtc = [DateTime]::UtcNow.ToString("o")
      State = "running"
      WorkingSetBytes = [int64]$Process.WorkingSet64
      PrivateMemoryBytes = [int64]$Process.PrivateMemorySize64
      HandleCount = $handleCount
    }
  }
  catch {
    return [pscustomobject]@{
      TimestampUtc = [DateTime]::UtcNow.ToString("o")
      State = "sample-error"
      WorkingSetBytes = $null
      PrivateMemoryBytes = $null
      HandleCount = $null
    }
  }
}

function Get-Percentile {
  param(
    [double[]]$Values,
    [double]$Percentile
  )

  if ($null -eq $Values -or $Values.Count -eq 0) {
    return $null
  }
  try {
    $sorted = [double[]]$Values.Clone()
    [Array]::Sort($sorted)
    $index = [Math]::Max(0, [Math]::Min($sorted.Count - 1, [int][Math]::Ceiling($sorted.Count * $Percentile) - 1))
    return [Math]::Round($sorted[$index], 3)
  }
  catch {
    throw "Unable to calculate percentile $Percentile from $($Values.Count) latency samples: $($_.Exception.Message)"
  }
}

function Get-SampleSummary {
  param([object[]]$Samples)

  $running = @($Samples | Where-Object { $_.State -eq "running" })
  if ($running.Count -eq 0) {
    return [pscustomobject]@{ SampleCount = 0 }
  }
  $first = $running[0]
  $last = $running[$running.Count - 1]
  $workingSets = @($running | ForEach-Object { [int64]$_.WorkingSetBytes })
  $privateBytes = @($running | ForEach-Object { [int64]$_.PrivateMemoryBytes })
  $handles = @($running | Where-Object { $null -ne $_.HandleCount } | ForEach-Object { [int64]$_.HandleCount })
  return [pscustomobject]@{
    SampleCount = $running.Count
    FirstWorkingSetBytes = [int64]$first.WorkingSetBytes
    LastWorkingSetBytes = [int64]$last.WorkingSetBytes
    PeakWorkingSetBytes = [int64](($workingSets | Measure-Object -Maximum).Maximum)
    WorkingSetDeltaBytes = [int64]$last.WorkingSetBytes - [int64]$first.WorkingSetBytes
    FirstPrivateMemoryBytes = [int64]$first.PrivateMemoryBytes
    LastPrivateMemoryBytes = [int64]$last.PrivateMemoryBytes
    PeakPrivateMemoryBytes = [int64](($privateBytes | Measure-Object -Maximum).Maximum)
    PrivateMemoryDeltaBytes = [int64]$last.PrivateMemoryBytes - [int64]$first.PrivateMemoryBytes
    FirstHandleCount = if ($handles.Count -gt 0) { [int64]$handles[0] } else { $null }
    LastHandleCount = if ($handles.Count -gt 0) { [int64]$handles[$handles.Count - 1] } else { $null }
    PeakHandleCount = if ($handles.Count -gt 0) { [int64](($handles | Measure-Object -Maximum).Maximum) } else { $null }
    HandleDelta = if ($handles.Count -gt 0) { [int64]$handles[$handles.Count - 1] - [int64]$handles[0] } else { $null }
  }
}

function Invoke-ConcurrentReadWorkload {
  param(
    [string]$BaseUri,
    [string]$DesktopToken,
    [string]$Path,
    [bool]$Authenticated,
    [int]$WorkerCount,
    [int]$Duration,
    [int]$TimeoutSeconds,
    [int]$SampleInterval,
    [int]$WorkerPollInterval,
    [int]$LatencyLimit,
    [System.Diagnostics.Process]$BackendProcess
  )

  $workerScript = @'
param(
  [string]$BaseUri,
  [string]$DesktopToken,
  [string]$Path,
  [bool]$Authenticated,
  [string]$DeadlineUtc,
  [int]$TimeoutSeconds,
  [int]$LatencyLimit
)

$ErrorActionPreference = "Stop"
try { Add-Type -AssemblyName System.Net.Http -ErrorAction Stop } catch {}
$client = [System.Net.Http.HttpClient]::new()
$client.Timeout = [TimeSpan]::FromSeconds($TimeoutSeconds)
$deadline = [DateTime]::Parse($DeadlineUtc, [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::RoundtripKind)
$latencies = New-Object System.Collections.Generic.List[double]
$random = [System.Random]::new()
$latencyObserved = 0
$maxLatencyMs = 0.0
$failureSamples = New-Object System.Collections.Generic.List[string]
$requests = 0
$successes = 0
$failures = 0

try {
  while ([DateTime]::UtcNow -lt $deadline) {
    $request = [System.Net.Http.HttpRequestMessage]::new([System.Net.Http.HttpMethod]::Get, "$BaseUri$Path")
    $response = $null
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    try {
      if ($Authenticated) {
        [void]$request.Headers.TryAddWithoutValidation("Authorization", "Bearer $DesktopToken")
      }
      $response = $client.SendAsync($request).GetAwaiter().GetResult()
      [void]$response.Content.ReadAsByteArrayAsync().GetAwaiter().GetResult()
      $latencyMs = $timer.Elapsed.TotalMilliseconds
      $latencyObserved++
      if ($latencyMs -gt $maxLatencyMs) { $maxLatencyMs = $latencyMs }
      if ($latencies.Count -lt $LatencyLimit) {
        $latencies.Add($latencyMs)
      }
      else {
        $replacementIndex = $random.Next($latencyObserved)
        if ($replacementIndex -lt $LatencyLimit) {
          $latencies[$replacementIndex] = $latencyMs
        }
      }
      $requests++
      if ([int]$response.StatusCode -ge 200 -and [int]$response.StatusCode -lt 300) {
        $successes++
      }
      else {
        $failures++
        if ($failureSamples.Count -lt 5) {
          $failureSamples.Add("HTTP:$([int]$response.StatusCode)")
        }
      }
    }
    catch {
      $latencyMs = $timer.Elapsed.TotalMilliseconds
      $latencyObserved++
      if ($latencyMs -gt $maxLatencyMs) { $maxLatencyMs = $latencyMs }
      if ($latencies.Count -lt $LatencyLimit) {
        $latencies.Add($latencyMs)
      }
      else {
        $replacementIndex = $random.Next($latencyObserved)
        if ($replacementIndex -lt $LatencyLimit) {
          $latencies[$replacementIndex] = $latencyMs
        }
      }
      $requests++
      $failures++
      if ($failureSamples.Count -lt 5) {
        $failureSamples.Add($_.Exception.GetType().Name)
      }
    }
    finally {
      if ($null -ne $response) { $response.Dispose() }
      $request.Dispose()
    }
  }
}
finally {
  $client.Dispose()
}

[pscustomobject]@{
  Requests = $requests
  Successes = $successes
  Failures = $failures
  LatenciesMs = @($latencies)
  LatencyObserved = $latencyObserved
  MaxLatencyMs = $maxLatencyMs
  FailureSamples = @($failureSamples)
}
'@

  $pool = [RunspaceFactory]::CreateRunspacePool(1, $WorkerCount)
  $pool.Open()
  $workers = @()
  $deadline = [DateTime]::UtcNow.AddSeconds($Duration).ToString("o")
  $samples = New-Object System.Collections.Generic.List[object]

  try {
    for ($index = 0; $index -lt $WorkerCount; $index++) {
      $powerShell = [PowerShell]::Create()
      $powerShell.RunspacePool = $pool
      [void]$powerShell.AddScript($workerScript)
      [void]$powerShell.AddParameter("BaseUri", $BaseUri)
      [void]$powerShell.AddParameter("DesktopToken", $DesktopToken)
      [void]$powerShell.AddParameter("Path", $Path)
      [void]$powerShell.AddParameter("Authenticated", $Authenticated)
      [void]$powerShell.AddParameter("DeadlineUtc", $deadline)
      [void]$powerShell.AddParameter("TimeoutSeconds", $TimeoutSeconds)
      [void]$powerShell.AddParameter("LatencyLimit", $LatencyLimit)
      $workers += [pscustomobject]@{
        PowerShell = $powerShell
        Handle = $powerShell.BeginInvoke()
      }
    }

    $nextSample = [DateTime]::UtcNow
    while (@($workers | Where-Object { -not $_.Handle.IsCompleted }).Count -gt 0) {
      if ([DateTime]::UtcNow -ge $nextSample) {
        $samples.Add((Get-ProcessSample -Process $BackendProcess))
        $nextSample = [DateTime]::UtcNow.AddSeconds($SampleInterval)
      }
      Start-Sleep -Milliseconds $WorkerPollInterval
    }
    $samples.Add((Get-ProcessSample -Process $BackendProcess))

    $results = @()
    foreach ($worker in $workers) {
      try {
        $results += @($worker.PowerShell.EndInvoke($worker.Handle))
      }
      catch {
        $results += [pscustomobject]@{
          Requests = 0
          Successes = 0
          Failures = 1
          LatenciesMs = @()
          LatencyObserved = 0
          MaxLatencyMs = 0.0
          FailureSamples = @("worker:$($_.Exception.GetType().Name)")
        }
      }
    }
  }
  finally {
    foreach ($worker in $workers) {
      $worker.PowerShell.Dispose()
    }
    $pool.Close()
    $pool.Dispose()
  }

  $latencies = New-Object System.Collections.Generic.List[double]
  $failureSamples = New-Object System.Collections.Generic.List[string]
  $requestCount = 0
  $successCount = 0
  $failureCount = 0
  $latencyObserved = 0
  $maxLatencyMs = 0.0
  foreach ($result in $results) {
    $requestCount += [int64]$result.Requests
    $successCount += [int64]$result.Successes
    $failureCount += [int64]$result.Failures
    $latencyObserved += [int64]$result.LatencyObserved
    if ([double]$result.MaxLatencyMs -gt $maxLatencyMs) {
      $maxLatencyMs = [double]$result.MaxLatencyMs
    }
    foreach ($latency in @($result.LatenciesMs)) {
      $latencies.Add([double]$latency)
    }
    foreach ($sample in @($result.FailureSamples)) {
      if ($failureSamples.Count -lt 10) {
        $failureSamples.Add([string]$sample)
      }
    }
  }

  $elapsedSeconds = $Duration
  $latencyValues = [double[]]$latencies.ToArray()
  $p50 = Get-Percentile -Values $latencyValues -Percentile 0.50
  $p95 = Get-Percentile -Values $latencyValues -Percentile 0.95
  $p99 = Get-Percentile -Values $latencyValues -Percentile 0.99
  $requestsPerSecond = if ($elapsedSeconds -gt 0) {
    [Math]::Round([double]$requestCount / [double]$elapsedSeconds, 3)
  }
  else {
    $null
  }
  $latencyMethod = if ($latencyObserved -gt $latencies.Count) { "reservoir-sample" } else { "all-observations" }
  $latencySummary = [ordered]@{
    Method = $latencyMethod
    ObservedCount = $latencyObserved
    RetainedSampleCount = $latencies.Count
    P50 = $p50
    P95 = $p95
    P99 = $p99
    # The retained reservoir drives percentiles. Max is a string-free numeric
    # aggregate that avoids retaining every latency during a long soak.
    Max = $maxLatencyMs
  }
  try {
    try {
      $resourceSummary = Get-SampleSummary -Samples $samples.ToArray()
    }
    catch {
      throw "Unable to summarize resource samples: $($_.Exception.Message)"
    }
    $workload = [ordered]@{
      Path = $Path
      Authenticated = $Authenticated
      DurationSeconds = $elapsedSeconds
      Concurrency = $WorkerCount
      Requests = $requestCount
      Successes = $successCount
      Failures = $failureCount
      RequestsPerSecond = $requestsPerSecond
      LatencyMs = [pscustomobject]$latencySummary
      FailureSamples = @($failureSamples)
      ResourceSamples = $samples.ToArray()
      ResourceSummary = $resourceSummary
    }
  }
  catch {
    throw "Unable to construct workload result; latency=$($latencies.GetType().FullName), samples=$($samples.GetType().FullName), resultCount=$($results.Count): $($_.Exception.Message)"
  }
  return [pscustomobject]$workload
}

function Assert-Equal {
  param(
    $Actual,
    $Expected,
    [string]$Message
  )
  if ($Actual -ne $Expected) {
    throw "$Message Expected '$Expected', got '$Actual'."
  }
}

function Assert-ReportContainsNoSecrets {
  param(
    [string]$SerializedReport,
    [AllowEmptyCollection()]
    [string[]]$DesktopTokens
  )

  foreach ($token in @($DesktopTokens)) {
    if (-not [string]::IsNullOrEmpty($token) -and $SerializedReport.Contains($token)) {
      throw "Refusing to expose a report containing a generated desktop token."
    }
  }
  if ($SerializedReport -match '(?i)\bBearer\s+[^\s"\\]+') {
    throw "Refusing to expose a report containing a Bearer credential."
  }
}

function Invoke-BoundaryChecks {
  param(
    $Server,
    [System.Net.Http.HttpClient]$Client,
    [string]$DesktopToken
  )

  $health = Invoke-BackendRequest -Client $Client -Method "GET" -Uri "$($Server.BaseUri)/health" -Headers @{}
  Assert-Equal -Actual $health.StatusCode -Expected 200 -Message "Unauthenticated health check failed."
  $healthPayload = $health.Body | ConvertFrom-Json
  if ($healthPayload.status -ne "ok") {
    throw "Health payload did not contain status=ok."
  }

  $unauthorized = Invoke-BackendRequest -Client $Client -Method "GET" -Uri "$($Server.BaseUri)/api/v1/capabilities" -Headers @{}
  Assert-Equal -Actual $unauthorized.StatusCode -Expected 401 -Message "Unauthenticated protected request was not rejected."
  $challenge = $unauthorized.Headers["WWW-Authenticate"]
  if ([string]::IsNullOrWhiteSpace($challenge) -or -not $challenge.StartsWith("Bearer")) {
    throw "Unauthenticated protected request did not return a Bearer challenge."
  }

  $authenticated = Invoke-BackendRequest -Client $Client -Method "GET" -Uri "$($Server.BaseUri)/api/v1/capabilities" -Headers @{ Authorization = "Bearer $DesktopToken" }
  Assert-Equal -Actual $authenticated.StatusCode -Expected 200 -Message "Authenticated capabilities request failed."
  $capabilities = $authenticated.Body | ConvertFrom-Json
  if ($capabilities.contractVersion -ne "v1") {
    throw "Capabilities response did not declare contractVersion=v1."
  }

  $allowedPreflight = Invoke-BackendRequest -Client $Client -Method "OPTIONS" -Uri "$($Server.BaseUri)/api/v1/capabilities" -Headers @{
    Origin = "tauri://localhost"
    "Access-Control-Request-Method" = "GET"
    "Access-Control-Request-Headers" = "authorization"
  }
  if ($allowedPreflight.StatusCode -lt 200 -or $allowedPreflight.StatusCode -ge 300) {
    throw "Allowed-origin CORS preflight returned HTTP $($allowedPreflight.StatusCode)."
  }
  Assert-Equal -Actual $allowedPreflight.Headers["Access-Control-Allow-Origin"] -Expected "tauri://localhost" -Message "Allowed origin was not reflected by CORS."

  $untrustedPreflight = Invoke-BackendRequest -Client $Client -Method "OPTIONS" -Uri "$($Server.BaseUri)/api/v1/capabilities" -Headers @{
    Origin = "https://untrusted.example.invalid"
    "Access-Control-Request-Method" = "GET"
    "Access-Control-Request-Headers" = "authorization"
  }
  if (-not [string]::IsNullOrEmpty($untrustedPreflight.Headers["Access-Control-Allow-Origin"])) {
    throw "Untrusted origin unexpectedly received Access-Control-Allow-Origin."
  }

  return [pscustomobject]@{
    HealthStatus = $health.StatusCode
    ProtectedWithoutBearerStatus = $unauthorized.StatusCode
    ProtectedWithBearerStatus = $authenticated.StatusCode
    AllowedOriginPreflightStatus = $allowedPreflight.StatusCode
    UntrustedOriginPreflightStatus = $untrustedPreflight.StatusCode
    Service = $healthPayload.service
    BackendVersion = $healthPayload.version
  }
}

function Invoke-ManagedFaultCheck {
  param(
    $Server,
    [System.Net.Http.HttpClient]$Client,
    [string]$Binary,
    [string]$HermesHome,
    [string]$DesktopToken,
    [System.Collections.Generic.List[string]]$DesktopTokens,
    [string]$WorkingDirectory,
    [int]$StartupTimeoutSeconds,
    [int]$StartupPollMilliseconds,
    [int]$StartupHandshakeMaxBytes,
    [int]$ShutdownGraceSeconds,
    [int]$KillTimeoutSeconds,
    [int]$FaultProbeTimeoutSeconds,
    [int]$FaultProbeAttempts,
    [int]$FaultProbeDelayMilliseconds
  )

  $originalBaseUri = $Server.BaseUri
  $originalPort = $Server.Port
  Stop-Backend -Server $Server -ShutdownGraceSeconds $ShutdownGraceSeconds `
    -KillTimeoutSeconds $KillTimeoutSeconds
  $connectionFailedAfterStop = $true
  $postStopClient = New-HttpClient -TimeoutSeconds $FaultProbeTimeoutSeconds
  try {
    for ($attempt = 1; $attempt -le $FaultProbeAttempts; $attempt++) {
      try {
        [void](Invoke-BackendRequest -Client $postStopClient -Method "GET" -Uri "$originalBaseUri/health" -Headers @{})
        $connectionFailedAfterStop = $false
        break
      }
      catch {}
      if ($attempt -lt $FaultProbeAttempts) {
        Start-Sleep -Milliseconds $FaultProbeDelayMilliseconds
      }
    }
  }
  finally {
    $postStopClient.Dispose()
  }
  if (-not $connectionFailedAfterStop) {
    throw "Managed backend termination did not make the old endpoint unavailable."
  }

  $restartServer = $null
  $restartToken = New-DesktopToken
  while ($restartToken -eq $DesktopToken) {
    $restartToken = New-DesktopToken
  }
  $DesktopTokens.Add($restartToken)
  try {
    $restartServer = Start-Backend -Binary $Binary -HermesHome $HermesHome `
      -DesktopToken $restartToken -WorkingDirectory $WorkingDirectory `
      -HandshakeTimeoutSeconds $StartupTimeoutSeconds `
      -HandshakeMaxBytes $StartupHandshakeMaxBytes `
      -PollMilliseconds $StartupPollMilliseconds `
      -ShutdownGrace $ShutdownGraceSeconds -KillTimeout $KillTimeoutSeconds
    $health = Wait-ForBackendHealth -Server $restartServer -Client $Client `
      -TimeoutSeconds $StartupTimeoutSeconds -PollMilliseconds $StartupPollMilliseconds `
      -DesktopTokens $DesktopTokens.ToArray()
    Assert-Equal -Actual $health.StatusCode -Expected 200 -Message "Restarted backend health check failed."
    $protected = Invoke-BackendRequest -Client $Client -Method "GET" -Uri "$($restartServer.BaseUri)/api/v1/capabilities" -Headers @{ Authorization = "Bearer $restartToken" }
    Assert-Equal -Actual $protected.StatusCode -Expected 200 -Message "Restarted backend did not accept its replacement token."
    $previousToken = Invoke-BackendRequest -Client $Client -Method "GET" -Uri "$($restartServer.BaseUri)/api/v1/capabilities" -Headers @{ Authorization = "Bearer $DesktopToken" }
    Assert-Equal -Actual $previousToken.StatusCode -Expected 401 -Message "Restarted backend accepted the previous generation token."
    return [pscustomobject]@{
      ManagedTerminationMadeOldEndpointUnavailable = $connectionFailedAfterStop
      TokenRotated = $restartToken -ne $DesktopToken
      PortChanged = $restartServer.Port -ne $originalPort
      RestartHealthPassed = $health.StatusCode -eq 200
      RestartAuthenticatedProbePassed = $protected.StatusCode -eq 200
      RestartRejectedPreviousToken = $previousToken.StatusCode -eq 401
      RestartedServer = $restartServer
    }
  }
  catch {
    Stop-Backend -Server $restartServer -ShutdownGraceSeconds $ShutdownGraceSeconds `
      -KillTimeoutSeconds $KillTimeoutSeconds
    throw
  }
}

Add-HttpAssembly
$workspace = Get-WorkspaceRoot
$resolvedCargo = Resolve-CargoExecutable -RequestedExecutable $CargoExecutable
$resolvedToolchain = Resolve-RustToolchain -RequestedToolchain $RustToolchain
$desktopToken = New-DesktopToken
$desktopTokens = New-Object System.Collections.Generic.List[string]
$desktopTokens.Add($desktopToken)
$startedAt = [DateTime]::UtcNow
$temporaryHermesHome = Join-Path ([System.IO.Path]::GetTempPath()) ("synthchat-phase4-" + [Guid]::NewGuid().ToString("N"))
$server = $null
$client = $null
$result = $null

try {
  [void][System.IO.Directory]::CreateDirectory($temporaryHermesHome)
  $binary = Resolve-BackendBinary -Workspace $workspace -RequestedBinary $BackendBinary `
    -SkipCompile $SkipBuild.IsPresent -Cargo $resolvedCargo -Toolchain $resolvedToolchain `
    -CargoTimeout $CargoTimeoutSeconds -ProcessKillTimeout $KillTimeoutSeconds
  $binaryHash = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash
  $client = New-HttpClient -TimeoutSeconds $RequestTimeoutSeconds

  $server = Start-Backend -Binary $binary -HermesHome $temporaryHermesHome `
    -DesktopToken $desktopToken -WorkingDirectory $workspace `
    -HandshakeTimeoutSeconds $StartupTimeoutSeconds `
    -HandshakeMaxBytes $StartupHandshakeMaxBytes `
    -PollMilliseconds $StartupPollMilliseconds `
    -ShutdownGrace $ShutdownGraceSeconds -KillTimeout $KillTimeoutSeconds
  $health = Wait-ForBackendHealth -Server $server -Client $client `
    -TimeoutSeconds $StartupTimeoutSeconds -PollMilliseconds $StartupPollMilliseconds `
    -DesktopTokens $desktopTokens.ToArray()
  $boundaryChecks = Invoke-BoundaryChecks -Server $server -Client $client -DesktopToken $desktopToken

  $healthWorkload = Invoke-ConcurrentReadWorkload -BaseUri $server.BaseUri -DesktopToken $desktopToken -Path "/health" -Authenticated $false -WorkerCount $Concurrency -Duration $DurationSeconds -TimeoutSeconds $RequestTimeoutSeconds -SampleInterval $SampleIntervalSeconds -WorkerPollInterval $WorkerPollMilliseconds -LatencyLimit $LatencySampleLimit -BackendProcess $server.Process
  $capabilitiesWorkload = Invoke-ConcurrentReadWorkload -BaseUri $server.BaseUri -DesktopToken $desktopToken -Path "/api/v1/capabilities" -Authenticated $true -WorkerCount $Concurrency -Duration $DurationSeconds -TimeoutSeconds $RequestTimeoutSeconds -SampleInterval $SampleIntervalSeconds -WorkerPollInterval $WorkerPollMilliseconds -LatencyLimit $LatencySampleLimit -BackendProcess $server.Process

  $faultChecks = $null
  if ($IncludeFaultChecks) {
    $faultChecks = Invoke-ManagedFaultCheck -Server $server -Client $client `
      -Binary $binary -HermesHome $temporaryHermesHome -DesktopToken $desktopToken `
      -DesktopTokens $desktopTokens -WorkingDirectory $workspace `
      -StartupTimeoutSeconds $StartupTimeoutSeconds `
      -StartupPollMilliseconds $StartupPollMilliseconds `
      -StartupHandshakeMaxBytes $StartupHandshakeMaxBytes `
      -ShutdownGraceSeconds $ShutdownGraceSeconds -KillTimeoutSeconds $KillTimeoutSeconds `
      -FaultProbeTimeoutSeconds $FaultProbeTimeoutSeconds `
      -FaultProbeAttempts $FaultProbeAttempts `
      -FaultProbeDelayMilliseconds $FaultProbeDelayMilliseconds
    $server = $faultChecks.RestartedServer
    $faultChecks.PSObject.Properties.Remove("RestartedServer")
  }

  $result = [ordered]@{
    SchemaVersion = 2
    StartedAtUtc = $startedAt.ToString("o")
    CompletedAtUtc = [DateTime]::UtcNow.ToString("o")
    Environment = [ordered]@{
      OS = [System.Environment]::OSVersion.VersionString
      PowerShellVersion = $PSVersionTable.PSVersion.ToString()
      ProcessorCount = [System.Environment]::ProcessorCount
      TemporaryHermesHome = if ($KeepHermesHome) { "retained-by-request" } else { "created-and-removed" }
    }
    Backend = [ordered]@{
      BinaryName = [System.IO.Path]::GetFileName($binary)
      BinarySha256 = $binaryHash
      Service = $boundaryChecks.Service
      Version = $boundaryChecks.BackendVersion
      BoundAddress = "127.0.0.1"
      OsAssignedPort = $true
      TokenReceivedThroughStdin = $true
      InheritedTokenRemoved = $true
      CargoToolchainOverrideUsed = -not [string]::IsNullOrWhiteSpace($resolvedToolchain)
    }
    Parameters = [ordered]@{
      DurationSecondsPerWorkload = $DurationSeconds
      ConcurrencyPerWorkload = $Concurrency
      SampleIntervalSeconds = $SampleIntervalSeconds
      RequestTimeoutSeconds = $RequestTimeoutSeconds
      CargoTimeoutSeconds = $CargoTimeoutSeconds
      StartupTimeoutSeconds = $StartupTimeoutSeconds
      StartupPollMilliseconds = $StartupPollMilliseconds
      WorkerPollMilliseconds = $WorkerPollMilliseconds
      StartupHandshakeMaxBytes = $StartupHandshakeMaxBytes
      ShutdownGraceSeconds = $ShutdownGraceSeconds
      KillTimeoutSeconds = $KillTimeoutSeconds
      LatencySampleLimitPerWorker = $LatencySampleLimit
      IncludeFaultChecks = $IncludeFaultChecks.IsPresent
      FaultProbeTimeoutSeconds = $FaultProbeTimeoutSeconds
      FaultProbeAttempts = $FaultProbeAttempts
      FaultProbeDelayMilliseconds = $FaultProbeDelayMilliseconds
    }
    BoundaryChecks = $boundaryChecks
    Workloads = @($healthWorkload, $capabilitiesWorkload)
    FaultChecks = $faultChecks
  }

  $totalFailures = [int64]$healthWorkload.Failures + [int64]$capabilitiesWorkload.Failures
  if ($totalFailures -ne 0) {
    throw "Runtime workload recorded $totalFailures failed requests."
  }

  $absoluteResultPath = $null
  if (-not [string]::IsNullOrWhiteSpace($ResultPath)) {
    $candidateResultPath = if ([System.IO.Path]::IsPathRooted($ResultPath)) {
      $ResultPath
    }
    else {
      Join-Path $workspace $ResultPath
    }
    $absoluteResultPath = [System.IO.Path]::GetFullPath($candidateResultPath)
    $directory = Split-Path -Parent $absoluteResultPath
    [void][System.IO.Directory]::CreateDirectory($directory)
    $result["ResultPath"] = $absoluteResultPath
  }

  $json = $result | ConvertTo-Json -Depth 12
  Assert-ReportContainsNoSecrets -SerializedReport $json -DesktopTokens $desktopTokens.ToArray()
  if ($null -ne $absoluteResultPath) {
    Set-Content -LiteralPath $absoluteResultPath -Value $json -Encoding utf8
  }

  $result
}
catch {
  $message = Protect-Text -Value $_.Exception.Message -Secrets $desktopTokens.ToArray()
  $stack = Protect-Text -Value $_.ScriptStackTrace -Secrets $desktopTokens.ToArray()
  $diagnostics = Get-BackendDiagnostics -Server $server -DesktopTokens $desktopTokens.ToArray()
  if ($null -ne $diagnostics) {
    $diagnosticText = $diagnostics | ConvertTo-Json -Depth 5 -Compress
    throw "$message at $stack Backend diagnostics: $diagnosticText"
  }
  throw "$message at $stack"
}
finally {
  if ($null -ne $client) {
    $client.Dispose()
  }
  Stop-Backend -Server $server -ShutdownGraceSeconds $ShutdownGraceSeconds `
    -KillTimeoutSeconds $KillTimeoutSeconds
  if (-not $KeepHermesHome) {
    Remove-Item -LiteralPath $temporaryHermesHome -Recurse -Force -ErrorAction SilentlyContinue
  }
}

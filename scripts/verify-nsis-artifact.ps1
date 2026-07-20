[CmdletBinding()]
param(
  [string]$InstallerPath,
  [string]$SevenZipPath,
  [string]$DesktopBinaryPath,
  [string]$BackendBinaryPath,
  [datetime]$BuiltAfterUtc,
  [switch]$RequireSignature
)

<#
.SYNOPSIS
Verifies a SynthChat NSIS artifact without installing it.

.DESCRIPTION
The verifier uses 7-Zip's NSIS reader to test and extract the installer into a
fresh system-temp directory. It requires exactly one desktop executable and one
Rust backend sidecar, compares them with the release build inputs, rejects
legacy runtime/user-data paths, and scans extracted files for high-confidence
credential signatures. The temporary directory is removed only after its
resolved parent and generated name have been validated.

Use -RequireSignature only for a signed release candidate. It requires valid
Authenticode signatures on the installer and both executable payloads.
#>

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
  throw "NSIS artifact verification requires Windows."
}

$Workspace = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$TargetTriple = "x86_64-pc-windows-msvc"

function Resolve-WorkspaceFile {
  param(
    [string]$RequestedPath,
    [string]$DefaultRelativePath,
    [string]$Label
  )

  $candidate = if ([string]::IsNullOrWhiteSpace($RequestedPath)) {
    Join-Path $Workspace $DefaultRelativePath
  }
  elseif ([IO.Path]::IsPathRooted($RequestedPath)) {
    $RequestedPath
  }
  else {
    Join-Path $Workspace $RequestedPath
  }

  $resolved = Resolve-Path -LiteralPath $candidate -ErrorAction Stop
  $item = Get-Item -LiteralPath $resolved.Path -Force
  if (-not $item.PSIsContainer -and -not ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
    return $item.FullName
  }
  throw "$Label must be a regular non-reparse file: $candidate"
}

function Resolve-SevenZip {
  param([string]$RequestedPath)

  if (-not [string]::IsNullOrWhiteSpace($RequestedPath)) {
    $resolved = Resolve-Path -LiteralPath $RequestedPath -ErrorAction Stop
    $item = Get-Item -LiteralPath $resolved.Path -Force
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
      throw "7-Zip must be a regular non-reparse file: $RequestedPath"
    }
    return $item.FullName
  }

  $command = Get-Command 7z.exe -CommandType Application -ErrorAction SilentlyContinue |
    Select-Object -First 1
  if ($null -ne $command) {
    return $command.Source
  }

  $programFilesCandidate = Join-Path $env:ProgramFiles "7-Zip\7z.exe"
  if (Test-Path -LiteralPath $programFilesCandidate -PathType Leaf) {
    return (Resolve-Path -LiteralPath $programFilesCandidate).Path
  }
  throw "7-Zip was not found. Install 7-Zip 25.x or pass -SevenZipPath."
}

function Invoke-SevenZip {
  param(
    [string]$Executable,
    [string[]]$Arguments,
    [string]$Operation
  )

  $output = @(& $Executable @Arguments 2>&1 | ForEach-Object { "$_" })
  if ($LASTEXITCODE -ne 0) {
    $tail = @($output | Select-Object -Last 20) -join [Environment]::NewLine
    throw "7-Zip $Operation failed with exit code $LASTEXITCODE.`n$tail"
  }
  return $output
}

function Get-SignatureStatus {
  param([string]$Path)
  return [string](Get-AuthenticodeSignature -LiteralPath $Path).Status
}

function Get-ByteArraySha256 {
  param([byte[]]$Bytes)

  $sha256 = [Security.Cryptography.SHA256]::Create()
  try {
    return [BitConverter]::ToString($sha256.ComputeHash($Bytes)).Replace("-", "")
  }
  finally {
    $sha256.Dispose()
  }
}

function Get-NsisPatchedDesktopImage {
  param([string]$Path)

  $unknownToken = "__TAURI_BUNDLE_TYPE_VAR_UNK"
  $nsisToken = "__TAURI_BUNDLE_TYPE_VAR_NSS"
  if ($unknownToken.Length -ne $nsisToken.Length) {
    throw "Tauri bundle type markers must be equal length."
  }

  [byte[]]$bytes = [IO.File]::ReadAllBytes($Path)
  $singleByteEncoding = [Text.Encoding]::GetEncoding(28591)
  $text = $singleByteEncoding.GetString($bytes)
  $markerIndex = $text.IndexOf($unknownToken, [StringComparison]::Ordinal)
  if ($markerIndex -lt 0) {
    throw "Desktop source binary does not contain the Tauri unknown bundle marker."
  }
  if ($text.IndexOf($unknownToken, $markerIndex + $unknownToken.Length, [StringComparison]::Ordinal) -ge 0) {
    throw "Desktop source binary contains more than one Tauri unknown bundle marker."
  }

  [byte[]]$replacement = [Text.Encoding]::ASCII.GetBytes($nsisToken)
  [Array]::Copy($replacement, 0, $bytes, $markerIndex, $replacement.Length)
  return ,$bytes
}

function Assert-ByteRange {
  param(
    [long]$Offset,
    [long]$Count,
    [long]$Length,
    [string]$Label
  )

  if ($Offset -lt 0 -or $Count -lt 0 -or $Offset -gt $Length -or $Count -gt ($Length - $Offset)) {
    throw "$Label is outside the portable executable."
  }
}

function Get-AuthenticodeNormalizedPeSha256 {
  param([byte[]]$Bytes)

  if ($Bytes.Length -lt 64 -or $Bytes[0] -ne 0x4d -or $Bytes[1] -ne 0x5a) {
    throw "Desktop payload is not a valid PE image."
  }
  $peOffset = [BitConverter]::ToInt32($Bytes, 0x3c)
  Assert-ByteRange $peOffset 24 $Bytes.Length "PE header"
  if (
    $Bytes[$peOffset] -ne 0x50 -or
    $Bytes[$peOffset + 1] -ne 0x45 -or
    $Bytes[$peOffset + 2] -ne 0 -or
    $Bytes[$peOffset + 3] -ne 0
  ) {
    throw "Desktop payload has an invalid PE signature."
  }

  $optionalHeader = $peOffset + 24
  Assert-ByteRange $optionalHeader 2 $Bytes.Length "PE optional header"
  $magic = [BitConverter]::ToUInt16($Bytes, $optionalHeader)
  $dataDirectories = if ($magic -eq 0x10b) {
    $optionalHeader + 96
  }
  elseif ($magic -eq 0x20b) {
    $optionalHeader + 112
  }
  else {
    throw "Desktop payload has an unsupported PE optional-header magic."
  }
  $checksumOffset = $optionalHeader + 64
  $securityDirectory = $dataDirectories + 32
  Assert-ByteRange $checksumOffset 4 $Bytes.Length "PE checksum"
  Assert-ByteRange $securityDirectory 8 $Bytes.Length "PE security directory"

  $certificateOffset = [long][BitConverter]::ToUInt32($Bytes, $securityDirectory)
  $certificateSize = [long][BitConverter]::ToUInt32($Bytes, $securityDirectory + 4)
  [byte[]]$normalized = $Bytes.Clone()
  [Array]::Clear($normalized, $checksumOffset, 4)
  [Array]::Clear($normalized, $securityDirectory, 8)
  if ($certificateSize -eq 0) {
    if ($certificateOffset -ne 0) {
      throw "PE certificate offset is non-zero while its size is zero."
    }
    return Get-ByteArraySha256 $normalized
  }

  Assert-ByteRange $certificateOffset $certificateSize $normalized.Length "PE certificate table"
  if (($certificateOffset % 8) -ne 0) {
    throw "PE certificate table is not 8-byte aligned."
  }
  $certificateEnd = $certificateOffset + $certificateSize
  [byte[]]$withoutCertificate = New-Object byte[] ($normalized.Length - $certificateSize)
  [Array]::Copy($normalized, 0, $withoutCertificate, 0, [int]$certificateOffset)
  $tailBytes = $normalized.Length - $certificateEnd
  if ($tailBytes -gt 0) {
    [Array]::Copy(
      $normalized,
      [int]$certificateEnd,
      $withoutCertificate,
      [int]$certificateOffset,
      [int]$tailBytes
    )
  }
  return Get-ByteArraySha256 $withoutCertificate
}

$Installer = Resolve-WorkspaceFile $InstallerPath `
  "desktop\target\release\bundle\nsis\SynthChat_1.1.0_x64-setup.exe" `
  "Installer"
$DesktopSource = Resolve-WorkspaceFile $DesktopBinaryPath `
  "desktop\target\release\synthchat-desktop.exe" `
  "Desktop source binary"
$BackendSource = Resolve-WorkspaceFile $BackendBinaryPath `
  "desktop\binaries\synthchat-hermes-backend-$TargetTriple.exe" `
  "Backend source binary"
$SevenZip = Resolve-SevenZip $SevenZipPath

if ($PSBoundParameters.ContainsKey("BuiltAfterUtc")) {
  $installerTime = (Get-Item -LiteralPath $Installer).LastWriteTimeUtc
  if ($installerTime -lt $BuiltAfterUtc.ToUniversalTime()) {
    throw "Installer predates the required build start: $installerTime"
  }
}

$listing = Invoke-SevenZip $SevenZip @("l", "-slt", "-bd", "--", $Installer) "listing"
if (($listing -join "`n") -notmatch '(?m)^Type = Nsis\s*$') {
  throw "7-Zip did not identify the artifact as an NSIS installer."
}
[void](Invoke-SevenZip $SevenZip @("t", "-bd", "--", $Installer) "integrity test")

$separatorCharacters = [char[]]@(
  [IO.Path]::DirectorySeparatorChar,
  [IO.Path]::AltDirectorySeparatorChar
)
$TempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd($separatorCharacters)
$ExtractDirectory = Join-Path $TempRoot ("synthchat-nsis-audit-" + [guid]::NewGuid().ToString("N"))
[void](New-Item -ItemType Directory -Path $ExtractDirectory)

try {
  [void](Invoke-SevenZip $SevenZip @("x", "-bd", "-y", "-o$ExtractDirectory", "--", $Installer) "extraction")

  $entries = @(Get-ChildItem -LiteralPath $ExtractDirectory -Recurse -Force)
  $reparseEntries = @($entries | Where-Object {
    $_.Attributes -band [IO.FileAttributes]::ReparsePoint
  })
  if ($reparseEntries.Count -gt 0) {
    throw "Extracted NSIS payload contains a reparse point."
  }

  $files = @($entries | Where-Object { -not $_.PSIsContainer })
  $relativePaths = @($files | ForEach-Object {
    $relative = [IO.Path]::GetRelativePath($ExtractDirectory, $_.FullName).Replace("\", "/")
    if ([IO.Path]::IsPathRooted($relative) -or $relative -eq ".." -or $relative.StartsWith("../")) {
      throw "Extracted payload escaped the audit directory: $relative"
    }
    $relative
  })

  $desktopPayload = @($files | Where-Object Name -ceq "synthchat-desktop.exe")
  $backendPayload = @($files | Where-Object Name -ceq "synthchat-hermes-backend.exe")
  if ($desktopPayload.Count -ne 1) {
    throw "Expected exactly one synthchat-desktop.exe; found $($desktopPayload.Count)."
  }
  if ($backendPayload.Count -ne 1) {
    throw "Expected exactly one synthchat-hermes-backend.exe; found $($backendPayload.Count)."
  }

  $forbiddenPath = [regex]::new(
    '(?i)(^|/)(src-tauri|synthchat-data|\.hermes|node_modules|\.multi-agent|agent|agents|mcp_servers|site-packages|__pycache__)(/|$)|(^|/)(python(?:w|3(?:\.\d+)?)?\.exe|python\d+\.dll|pyvenv\.cfg|pip(?:3)?\.exe)$|\.(?:db|sqlite|sqlite3|pem|key|pfx|p12)$|(^|/)\.env(?:\.|$)|(^|/)(?:credentials?|secrets?)(?:\.|/|$)',
    [Text.RegularExpressions.RegexOptions]::CultureInvariant,
    [TimeSpan]::FromSeconds(2)
  )
  $forbiddenPaths = @($relativePaths | Where-Object { $forbiddenPath.IsMatch($_) })
  if ($forbiddenPaths.Count -gt 0) {
    throw "Forbidden payload paths:`n$($forbiddenPaths -join "`n")"
  }

  [byte[]]$expectedDesktopImage = Get-NsisPatchedDesktopImage $DesktopSource
  [byte[]]$packagedDesktopImage = [IO.File]::ReadAllBytes($desktopPayload[0].FullName)
  $expectedDesktopHash = Get-ByteArraySha256 $expectedDesktopImage
  $packagedDesktopHash = Get-ByteArraySha256 $packagedDesktopImage
  $desktopSignature = Get-SignatureStatus $desktopPayload[0].FullName
  $desktopRelation = "tauri_nsis_bundle_type_patch"
  if ($expectedDesktopHash -ne $packagedDesktopHash) {
    if ($desktopSignature -ne "Valid") {
      throw "Extracted desktop payload differs from the expected Tauri NSIS bundle-type patch."
    }
    $expectedNormalizedHash = Get-AuthenticodeNormalizedPeSha256 $expectedDesktopImage
    $packagedNormalizedHash = Get-AuthenticodeNormalizedPeSha256 $packagedDesktopImage
    if ($expectedNormalizedHash -ne $packagedNormalizedHash) {
      throw "Signed desktop payload differs beyond the expected Tauri NSIS patch and Authenticode fields."
    }
    $desktopRelation = "tauri_nsis_bundle_type_patch_and_authenticode"
  }

  $backendSourceHash = (Get-FileHash -LiteralPath $BackendSource -Algorithm SHA256).Hash
  $backendPayloadHash = (Get-FileHash -LiteralPath $backendPayload[0].FullName -Algorithm SHA256).Hash
  if ($backendSourceHash -ne $backendPayloadHash) {
    throw "Extracted Rust backend payload does not match its release build input."
  }

  $payloadResults = @(
    [pscustomobject]@{
      source = $DesktopSource
      payload = [IO.Path]::GetRelativePath($ExtractDirectory, $desktopPayload[0].FullName).Replace("\", "/")
      sourceSha256 = (Get-FileHash -LiteralPath $DesktopSource -Algorithm SHA256).Hash
      packagedSha256 = $packagedDesktopHash
      relation = $desktopRelation
      bytes = $desktopPayload[0].Length
      signatureStatus = $desktopSignature
    },
    [pscustomobject]@{
      source = $BackendSource
      payload = [IO.Path]::GetRelativePath($ExtractDirectory, $backendPayload[0].FullName).Replace("\", "/")
      sourceSha256 = $backendSourceHash
      packagedSha256 = $backendPayloadHash
      relation = "exact"
      bytes = $backendPayload[0].Length
      signatureStatus = Get-SignatureStatus $backendPayload[0].FullName
    }
  )

  $secretPattern = [regex]::new(
    '(?i)-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_-]{35}|gh[pousr]_[0-9A-Za-z]{36,255}|sk-ant-[0-9A-Za-z_-]{20,}|sk-(?:proj-|svcacct-)?[0-9A-Za-z_-]{24,}',
    [Text.RegularExpressions.RegexOptions]::CultureInvariant,
    [TimeSpan]::FromSeconds(2)
  )
  $singleByteEncoding = [Text.Encoding]::GetEncoding(28591)
  $secretMatches = @()
  foreach ($file in $files) {
    $bytes = [IO.File]::ReadAllBytes($file.FullName)
    $singleByteText = $singleByteEncoding.GetString($bytes)
    $unicodeText = [Text.Encoding]::Unicode.GetString($bytes)
    if ($secretPattern.IsMatch($singleByteText) -or $secretPattern.IsMatch($unicodeText)) {
      $secretMatches += [IO.Path]::GetRelativePath($ExtractDirectory, $file.FullName).Replace("\", "/")
    }
  }
  if ($secretMatches.Count -gt 0) {
    throw "High-confidence credential material found in extracted payload: $($secretMatches -join ', ')"
  }

  $installerSignature = Get-SignatureStatus $Installer
  if ($RequireSignature) {
    $signatureResults = @(
      [pscustomobject]@{ path = $Installer; status = $installerSignature },
      [pscustomobject]@{ path = $desktopPayload[0].FullName; status = $desktopSignature },
      [pscustomobject]@{
        path = $backendPayload[0].FullName
        status = $payloadResults[1].signatureStatus
      }
    )
    $invalidSignatures = @($signatureResults | Where-Object status -ne "Valid")
    if ($invalidSignatures.Count -gt 0) {
      throw "Signed release verification requires valid Authenticode signatures on the installer and both executable payloads."
    }
  }

  [pscustomobject]@{
    schemaVersion = 1
    status = "passed"
    installer = $Installer
    installerSha256 = (Get-FileHash -LiteralPath $Installer -Algorithm SHA256).Hash
    installerBytes = (Get-Item -LiteralPath $Installer).Length
    installerSignatureStatus = $installerSignature
    extractedFileCount = $files.Count
    payloadPaths = @($relativePaths | Sort-Object)
    forbiddenPathCount = $forbiddenPaths.Count
    highConfidenceSecretMatches = $secretMatches.Count
    payloads = $payloadResults
  } | ConvertTo-Json -Depth 6
}
finally {
  if (Test-Path -LiteralPath $ExtractDirectory) {
    $resolvedExtract = [IO.Path]::GetFullPath($ExtractDirectory).TrimEnd($separatorCharacters)
    $resolvedParent = [IO.Path]::GetDirectoryName($resolvedExtract)
    $resolvedLeaf = [IO.Path]::GetFileName($resolvedExtract)
    if (
      -not $resolvedParent.Equals($TempRoot, [StringComparison]::OrdinalIgnoreCase) -or
      $resolvedLeaf -notmatch '^synthchat-nsis-audit-[0-9a-f]{32}$'
    ) {
      throw "Refusing unsafe audit cleanup: $resolvedExtract"
    }
    Remove-Item -LiteralPath $resolvedExtract -Recurse -Force
  }
}

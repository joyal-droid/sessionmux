# Copyright (c) 2026 joyal-droid. SPDX-License-Identifier: CPAL-1.0
#
# The contents of this file are subject to the Common Public Attribution License Version 1.0
# (the "License"); you may not use this file except in compliance with the License. You may
# obtain a copy of the License in the LICENSE file at the root of this repository or at
# https://opensource.org/license/cpal-1-0. The License is based on the Mozilla Public License
# Version 1.1 but Sections 14 and 15 have been added to cover use of software over a computer
# network and provide for limited attribution for the Original Developer. In addition, Exhibit A
# has been modified to be consistent with Exhibit B.
#
# Software distributed under the License is distributed on an "AS IS" basis, WITHOUT WARRANTY OF
# ANY KIND, either express or implied. See the License for the specific language governing rights
# and limitations under the License.
#
# The Original Code is Sessionmux. The Initial Developer of the Original Code is joyal-droid. All
# portions of the code written by joyal-droid are Copyright (c) 2026 joyal-droid. All Rights Reserved.
# Contributor(s): see the git history of this repository.
#
# Attribution (Exhibit B of the License): "Powered by Sessionmux", Copyright (c) 2026 joyal-droid,
# https://github.com/joyal-droid/sessionmux and the Sessionmux mark must stay visible in the user
# interface of every copy and derivative; see docs/attribution.md.

<#
.SYNOPSIS
  Builds Sessionmux (release) and packages it into dist\Sessionmux-Setup-<version>.exe with NSIS,
  optionally code-signing the app and the installer (signing is optional).

.DESCRIPTION
  1. puts %USERPROFILE%\.cargo\bin on PATH, sets SESSIONMUX_BUILD_DATE and SOURCE_DATE_EPOCH
     from the date of the checked-out commit (each unless already set; outside a git checkout the
     build date falls back to UTC today), sets RUSTFLAGS so that no absolute path of this machine
     ends up in the exe and the PE timestamp is deterministic (see below) and runs
     "cargo build --release --locked"
  2. scans Sessionmux.exe with scripts\check-binary-privacy.ps1 and stops on any hit
  3. stages Sessionmux.exe + README.md + LICENSE.txt (a copy of LICENSE) + THIRD-PARTY.md into
     dist\stage
     and scans the staged files and installer\sessionmux.nsi
  4. with -Sign: signs the staged Sessionmux.exe
  5. compiles installer\sessionmux.nsi once without compression into a temporary file and scans
     it (the real setup exe is LZMA-compressed, so its payload cannot be searched), deletes it,
     then compiles the real setup exe with makensis and scans it too
  6. with -Sign: signs the setup exe, then "signtool verify /pa" on both files
  7. prints the installer path, size and SHA-256

  Path remapping: rustc records source paths for panic messages, and for crates from the cargo
  registry those are absolute (C:\Users\<name>\.cargo\registry\...). The build therefore passes
    --remap-path-prefix=%USERPROFILE%=/home
    --remap-path-prefix=<CARGO_HOME or %USERPROFILE%\.cargo>=/cargo
    --remap-path-prefix=<RUSTUP_HOME or %USERPROFILE%\.rustup>=/rustc
    --remap-path-prefix=<repository root>=/src
    -Clink-arg=/DEBUG:NONE   (no debug directory, so no .pdb file name in the exe)
    -Clink-arg=/Brepro       (the linker writes a content hash instead of the current time as
                              the PE timestamp, so rebuilding the same commit gives the same exe)
  The last matching prefix wins, so the more specific ones come last. An existing RUSTFLAGS
  value is kept in front of these. Cargo rebuilds everything when the flags change, so the first
  build after a plain "cargo build --release" is a full one. A plain cargo build is not
  privacy-clean; only builds made by this script or by CI are.

  makensis is looked up in this order: -Makensis, $env:MAKENSIS, makensis on PATH, then a
  standard NSIS install in Program Files.

  Signing (-Sign) is optional; unsigned installers work but SmartScreen warns about them.
  It needs a code-signing certificate, chosen from the environment:
    $env:SESSIONMUX_SIGN_THUMBPRINT   SHA-1 thumbprint of a certificate in CurrentUser\My
                                     (also works for hardware tokens / EV certificates)
  or
    $env:SESSIONMUX_SIGN_PFX          path to a .pfx file
    $env:SESSIONMUX_SIGN_PFX_PASSWORD its password
  signtool.exe: $env:SIGNTOOL, else the highest
  ${env:ProgramFiles(x86)}\Windows Kits\10\bin\<version>\x64\signtool.exe, else signtool on PATH.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File installer\build.ps1
.EXAMPLE
  powershell -File installer\build.ps1 -Makensis "C:\Program Files (x86)\NSIS\makensis.exe" -SkipBuild
.EXAMPLE
  $env:SESSIONMUX_SIGN_THUMBPRINT = "0123...ABCD"; powershell -File installer\build.ps1 -Sign
#>
[CmdletBinding()]
param(
  # Full path to makensis.exe (overrides every other lookup).
  [string]$Makensis,
  # Package the existing release Sessionmux.exe without running cargo.
  [switch]$SkipBuild,
  # Code-sign Sessionmux.exe and the setup exe (certificate from the environment, see above).
  [switch]$Sign,
  # RFC 3161 timestamp server used with -Sign.
  [string]$TimestampUrl = 'http://timestamp.digicert.com'
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

function Fail([string]$msg) {
  Write-Host "build.ps1: $msg" -ForegroundColor Red
  exit 1
}

$Root = Split-Path -Parent $PSScriptRoot          # sessionmux\
$Nsi = Join-Path $PSScriptRoot 'sessionmux.nsi'
$Dist = Join-Path $Root 'dist'
$Stage = Join-Path $Dist 'stage'
$ScanScript = Join-Path $Root 'scripts\check-binary-privacy.ps1'
if (-not (Test-Path -LiteralPath $ScanScript)) { Fail "scripts\check-binary-privacy.ps1 not found at $ScanScript" }

# Runs the privacy scan on one file and stops the build on any hit.
function Invoke-PrivacyScan([string]$file) {
  Write-Host "Privacy scan: $(Split-Path -Leaf $file) ..."
  & $ScanScript $file
  $code = $LASTEXITCODE
  if ($code -ne 0) { Fail "privacy scan failed for $file (exit $code); see the hits above" }
}

# The --remap-path-prefix flags described above, built from this machine's folders.
function Get-ReleaseRustflags {
  $userProfile = if ($env:USERPROFILE) { $env:USERPROFILE } else { [Environment]::GetFolderPath('UserProfile') }
  $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $userProfile '.cargo' }
  $rustupHome = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $userProfile '.rustup' }
  $pairs = @(
    @($userProfile, '/home'),
    @($cargoHome, '/cargo'),
    @($rustupHome, '/rustc'),
    @($Root, '/src')
  )
  $flags = @()
  foreach ($pair in $pairs) {
    $from = [System.IO.Path]::GetFullPath($pair[0]).TrimEnd('\')
    # RUSTFLAGS is split on spaces.
    if ($from -match '\s') { Fail "cannot remap a path that contains spaces through RUSTFLAGS: $from" }
    $flags += "--remap-path-prefix=$from=$($pair[1])"
  }
  $flags += '-Clink-arg=/DEBUG:NONE'
  $flags += '-Clink-arg=/Brepro'
  return ($flags -join ' ')
}

# ---------------------------------------------------------------- version (Cargo.toml)
$Version = $null
$FileVersion = $null
foreach ($line in Get-Content -LiteralPath (Join-Path $Root 'Cargo.toml')) {
  # first `version = "x.y.z"` or `version = "x.y.z-pre"` line = [package] version (dependencies
  # are inline tables)
  if ($line -match '^\s*version\s*=\s*"(([0-9]+\.[0-9]+\.[0-9]+)(-[0-9A-Za-z.-]+)?)"') {
    $Version = $Matches[1]
    # Windows file versions are four numbers: 1.0.0 -> 1.0.0.0, 1.1.0-preview.1 -> 1.1.0.0.
    $FileVersion = "$($Matches[2]).0"
    break
  }
}
if (-not $Version) { Fail 'could not read the package version from Cargo.toml' }

# ---------------------------------------------------------------- signing setup (fail early)
function Find-Signtool {
  if ($env:SIGNTOOL -and (Test-Path -LiteralPath $env:SIGNTOOL)) { return $env:SIGNTOOL }
  $pf86 = ${env:ProgramFiles(x86)}
  if ($pf86) {
    $kits = Join-Path $pf86 'Windows Kits\10\bin'
    if (Test-Path -LiteralPath $kits) {
      $best = Get-ChildItem -LiteralPath $kits -Directory |
        Where-Object { $_.Name -match '^\d+(\.\d+)+$' -and (Test-Path -LiteralPath (Join-Path $_.FullName 'x64\signtool.exe')) } |
        Sort-Object { [version]$_.Name } -Descending |
        Select-Object -First 1
      if ($best) { return (Join-Path $best.FullName 'x64\signtool.exe') }
    }
  }
  $onPath = Get-Command signtool -ErrorAction SilentlyContinue
  if ($onPath) { return $onPath.Source }
  return $null
}

$Signtool = $null
$CertArgs = @()
$CertDesc = ''
if ($Sign) {
  $Signtool = Find-Signtool
  if (-not $Signtool) { Fail 'signtool.exe not found. Install the Windows 10/11 SDK (Signing Tools) or set $env:SIGNTOOL.' }
  if ($env:SESSIONMUX_SIGN_THUMBPRINT) {
    $tp = ($env:SESSIONMUX_SIGN_THUMBPRINT -replace '[^0-9A-Fa-f]', '')
    if ($tp.Length -ne 40) { Fail 'SESSIONMUX_SIGN_THUMBPRINT must be a 40-character SHA-1 thumbprint' }
    if (-not (Test-Path -LiteralPath "Cert:\CurrentUser\My\$tp")) { Fail "no certificate with thumbprint $tp in CurrentUser\My" }
    $CertArgs = @('/sha1', $tp, '/s', 'My')
    $CertDesc = "CurrentUser\My $tp"
  } elseif ($env:SESSIONMUX_SIGN_PFX) {
    if (-not (Test-Path -LiteralPath $env:SESSIONMUX_SIGN_PFX)) { Fail "SESSIONMUX_SIGN_PFX not found: $env:SESSIONMUX_SIGN_PFX" }
    $CertArgs = @('/f', $env:SESSIONMUX_SIGN_PFX)
    if ($env:SESSIONMUX_SIGN_PFX_PASSWORD) { $CertArgs += @('/p', $env:SESSIONMUX_SIGN_PFX_PASSWORD) }
    $CertDesc = "PFX $env:SESSIONMUX_SIGN_PFX"
  } else {
    Fail '-Sign needs $env:SESSIONMUX_SIGN_THUMBPRINT, or $env:SESSIONMUX_SIGN_PFX (+ $env:SESSIONMUX_SIGN_PFX_PASSWORD)'
  }
  Write-Host "Signing with $CertDesc via $Signtool (timestamp $TimestampUrl)"
}

function Invoke-Sign([string]$file) {
  # The password is never echoed: only the file name is printed.
  Write-Host "Signing $(Split-Path -Leaf $file) ..."
  $ErrorActionPreference = 'Continue'
  & $Signtool sign /fd SHA256 /td SHA256 /tr $TimestampUrl @CertArgs /d 'Sessionmux' $file
  $code = $LASTEXITCODE
  $ErrorActionPreference = 'Stop'
  if ($code -ne 0) { Fail "signtool sign failed for $file (exit $code)" }
}

function Invoke-Verify([string]$file) {
  $ErrorActionPreference = 'Continue'
  & $Signtool verify /pa $file
  $code = $LASTEXITCODE
  $ErrorActionPreference = 'Stop'
  if ($code -ne 0) { Fail "signtool verify /pa failed for $file (exit $code)" }
}

# ---------------------------------------------------------------- cargo build --release
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
Push-Location $Root
try {
  if (-not $SkipBuild) {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { Fail 'cargo not found (install Rust from https://rustup.rs)' }
    # The build date shown in the About card. build.rs re-runs when this variable changes, so
    # the exe never carries a stale date. SOURCE_DATE_EPOCH, when set, decides it; otherwise, in
    # this repository's own git checkout, SOURCE_DATE_EPOCH becomes the timestamp of the commit
    # being built, so rebuilding a tag later gives the same exe. A copy of the source that sits
    # inside some other git work tree does not take that tree's commit date.
    if (-not $env:SOURCE_DATE_EPOCH -and (Get-Command git -ErrorAction SilentlyContinue)) {
      $ErrorActionPreference = 'Continue'   # git writes to stderr outside a checkout
      try {
        $top = & git -C $Root rev-parse --show-toplevel 2>$null
        if ($LASTEXITCODE -eq 0 -and "$top".Trim()) {
          $topFull = [IO.Path]::GetFullPath("$top".Trim()).TrimEnd('\', '/')
          $rootFull = [IO.Path]::GetFullPath($Root).TrimEnd('\', '/')
          if ($topFull -ieq $rootFull) {
            $ct = & git -C $Root log -1 --format=%ct 2>$null
            if ($LASTEXITCODE -eq 0 -and "$ct".Trim() -match '^\d+$') { $env:SOURCE_DATE_EPOCH = "$ct".Trim() }
          }
        }
      } catch { }
      $ErrorActionPreference = 'Stop'
    }
    if ($env:SOURCE_DATE_EPOCH -and $env:SOURCE_DATE_EPOCH -notmatch '^\d+$') { Fail "SOURCE_DATE_EPOCH must be a number of seconds (got '$env:SOURCE_DATE_EPOCH')" }
    if (-not $env:SESSIONMUX_BUILD_DATE) {
      $env:SESSIONMUX_BUILD_DATE = if ($env:SOURCE_DATE_EPOCH) {
        # The About card shows a UTC date (%cs would use the committer's time zone).
        [DateTimeOffset]::FromUnixTimeSeconds([long]$env:SOURCE_DATE_EPOCH).UtcDateTime.ToString('yyyy-MM-dd')
      } else {
        (Get-Date).ToUniversalTime().ToString('yyyy-MM-dd')
      }
    }
    if ($env:SESSIONMUX_BUILD_DATE -notmatch '^\d{4}-\d{2}-\d{2}$') { Fail "SESSIONMUX_BUILD_DATE must be YYYY-MM-DD (got '$env:SESSIONMUX_BUILD_DATE')" }
    if ($env:CARGO_ENCODED_RUSTFLAGS) { Fail 'CARGO_ENCODED_RUSTFLAGS is set and would override the path remapping; unset it and use RUSTFLAGS.' }
    $remap = Get-ReleaseRustflags
    $env:RUSTFLAGS = if ($env:RUSTFLAGS) { "$($env:RUSTFLAGS.Trim()) $remap" } else { $remap }
    Write-Host "RUSTFLAGS: $env:RUSTFLAGS"
    if ($env:SOURCE_DATE_EPOCH) { Write-Host "SOURCE_DATE_EPOCH: $env:SOURCE_DATE_EPOCH" }
    Write-Host "Building Sessionmux $Version, build date $env:SESSIONMUX_BUILD_DATE (cargo build --release --locked)..."
    $ErrorActionPreference = 'Continue'   # cargo reports progress on stderr
    # --locked: build exactly the dependency versions in Cargo.lock, or fail.
    & cargo build --release --locked
    $ErrorActionPreference = 'Stop'
    if ($LASTEXITCODE -ne 0) { Fail "cargo build --release --locked failed (exit $LASTEXITCODE)" }
  } else {
    Write-Host 'Packaging the existing release build (-SkipBuild); its build date is whatever it was built with.'
  }

  # CARGO_TARGET_DIR or a cargo config may move target\:
  # ask cargo where the target directory really is, fall back to .\target.
  $TargetDir = Join-Path $Root 'target'
  if (Get-Command cargo -ErrorAction SilentlyContinue) {
    # Native stderr must not turn into terminating errors under 'Stop' (PowerShell 5.1).
    $ErrorActionPreference = 'Continue'
    try {
      $meta = & cargo metadata --format-version 1 --no-deps 2>$null
      if ($LASTEXITCODE -eq 0 -and $meta) { $TargetDir = ($meta | Out-String | ConvertFrom-Json).target_directory }
    } catch { }
    $ErrorActionPreference = 'Stop'
  }
  $Exe = Join-Path $TargetDir 'release\Sessionmux.exe'
  if (-not (Test-Path -LiteralPath $Exe)) { $Exe = Join-Path $Root 'target\release\Sessionmux.exe' }
  if (-not (Test-Path -LiteralPath $Exe)) { Fail "Sessionmux.exe not found in $TargetDir\release (run without -SkipBuild)" }
}
finally {
  Pop-Location
}

# No absolute path, machine name or personal name may ship (checked after -SkipBuild too).
Invoke-PrivacyScan $Exe

# ---------------------------------------------------------------- stage
# Exactly the four files the installer ships (THIRD-PARTY.md carries the crates' notices).
$Readme = Join-Path $Root 'README.md'
$License = Join-Path $Root 'LICENSE'
if (-not (Test-Path -LiteralPath $Readme)) { Fail "README.md not found at $Readme" }
if (-not (Test-Path -LiteralPath $License)) { Fail "LICENSE not found at $License (the installer ships it as LICENSE.txt; add the CPAL-1.0 text at the repository root)" }
$Notices = Join-Path $Root 'THIRD-PARTY.md'
if (-not (Test-Path -LiteralPath $Notices)) { Fail "THIRD-PARTY.md not found at $Notices" }
if (Test-Path -LiteralPath $Stage) { Remove-Item -LiteralPath $Stage -Recurse -Force }
New-Item -ItemType Directory -Force -Path $Stage | Out-Null
$StagedExe = Join-Path $Stage 'Sessionmux.exe'
Copy-Item -LiteralPath $Exe -Destination $StagedExe
# The installed README.md sits next to the exe without docs\ or the other Markdown files, so its
# relative links and images are rewritten to absolute repository URLs (taken from src\brand.rs).
$BrandRs = Get-Content -Raw -LiteralPath (Join-Path $Root 'src\brand.rs')
$RepoMatch = [regex]::Match($BrandRs, '"(https://github\.com/[^"]+)"')
if (-not $RepoMatch.Success) { Fail "repository URL not found in src\brand.rs" }
$RepoUrl = $RepoMatch.Groups[1].Value.TrimEnd('/')
$ReadmeText = Get-Content -Raw -Encoding UTF8 -LiteralPath $Readme
$ReadmeText = [regex]::Replace($ReadmeText, '(!?)(\[[^\]]*\])\((?![a-zA-Z][a-zA-Z0-9+.-]*:|#|/)([^)\s]+)\)', {
    param($m)
    $kind = if ($m.Groups[1].Value -eq '!') { 'raw' } else { 'blob' }
    '{0}{1}({2}/{3}/main/{4})' -f $m.Groups[1].Value, $m.Groups[2].Value, $RepoUrl, $kind, $m.Groups[3].Value
  })
[System.IO.File]::WriteAllText((Join-Path $Stage 'README.md'), $ReadmeText, (New-Object System.Text.UTF8Encoding $false))
Copy-Item -LiteralPath $License -Destination (Join-Path $Stage 'LICENSE.txt')
Copy-Item -LiteralPath $Notices -Destination (Join-Path $Stage 'THIRD-PARTY.md')
$staged = @(Get-ChildItem -LiteralPath $Stage -File | ForEach-Object { $_.Name } | Sort-Object)
if (($staged -join ',') -ne 'LICENSE.txt,README.md,Sessionmux.exe,THIRD-PARTY.md') { Fail "unexpected files in ${Stage}: $($staged -join ', ')" }
Write-Host ("Staged {0} ({1:N2} MB) + README.md + LICENSE.txt + THIRD-PARTY.md in {2}" -f $Exe, ((Get-Item -LiteralPath $Exe).Length / 1MB), $Stage)
# The setup exe is LZMA-compressed, so a scan of it cannot see these files: scan them (and the
# installer script) here, before anything is packed.
Invoke-PrivacyScan $Stage
Invoke-PrivacyScan $Nsi

# Sign the app before it is packed (the build output in the target dir stays untouched).
if ($Sign) { Invoke-Sign $StagedExe }

# ---------------------------------------------------------------- makensis
function Find-Makensis {
  $candidates = @()
  if ($Makensis) { return $Makensis }
  if ($env:MAKENSIS) { $candidates += $env:MAKENSIS }
  $onPath = Get-Command makensis -ErrorAction SilentlyContinue
  if ($onPath) { $candidates += $onPath.Source }
  foreach ($pf in @(${env:ProgramFiles(x86)}, $env:ProgramFiles)) {
    if ($pf) { $candidates += (Join-Path $pf 'NSIS\makensis.exe') }
  }
  foreach ($c in $candidates) { if ($c -and (Test-Path -LiteralPath $c)) { return $c } }
  return $null
}

$MakensisExe = Find-Makensis
if (-not $MakensisExe -or -not (Test-Path -LiteralPath $MakensisExe)) {
  Fail 'makensis.exe not found. Install NSIS 3 (https://nsis.sourceforge.io) or pass -Makensis <path>.'
}
# Some NSIS distributions keep makensis in Bin\ with Include\ and Plugins\ one level up;
# point NSISDIR there so MUI2, LogicLib and nsExec resolve.
$binDir = Split-Path -Parent $MakensisExe
if ((Split-Path -Leaf $binDir) -ieq 'Bin') {
  $nsisRoot = Split-Path -Parent $binDir
  if (Test-Path -LiteralPath (Join-Path $nsisRoot 'Include\MUI2.nsh')) { $env:NSISDIR = $nsisRoot }
}

$Out = Join-Path $Dist "Sessionmux-Setup-$Version.exe"
$Icon = Join-Path $Root 'assets\icon.ico'
if (Test-Path -LiteralPath $Out) { Remove-Item -LiteralPath $Out -Force }

function Invoke-Makensis([string]$outFile, [string[]]$extra) {
  $ErrorActionPreference = 'Continue'
  & $MakensisExe /V2 /INPUTCHARSET UTF8 @extra "/DVERSION=$Version" "/DFILE_VERSION=$FileVersion" "/DSTAGE_DIR=$Stage" "/DOUT_FILE=$outFile" "/DICON_FILE=$Icon" $Nsi
  $code = $LASTEXITCODE
  $ErrorActionPreference = 'Stop'
  if ($code -ne 0) { Fail "makensis failed (exit $code)" }
  if (-not (Test-Path -LiteralPath $outFile)) { Fail "makensis reported success but $outFile is missing" }
}

# An uncompressed build of the same script, so the scan sees the compiled installer strings and
# the packed files exactly as they are stored; it is deleted right after the scan.
$Plain = Join-Path $Dist 'scan-uncompressed.exe'
if (Test-Path -LiteralPath $Plain) { Remove-Item -LiteralPath $Plain -Force }
Write-Host "Compiling an uncompressed installer for the privacy scan ..."
Invoke-Makensis $Plain @('/DUNCOMPRESSED')
try {
  # A stored (uncompressed) copy is larger than the exe it carries; anything smaller is compressed.
  if ((Get-Item -LiteralPath $Plain).Length -le (Get-Item -LiteralPath $StagedExe).Length) {
    Fail "the uncompressed installer is smaller than Sessionmux.exe, so its contents would not be searchable"
  }
  Invoke-PrivacyScan $Plain
} finally { Remove-Item -LiteralPath $Plain -Force -ErrorAction SilentlyContinue }

Write-Host "Compiling installer with $MakensisExe ..."
Invoke-Makensis $Out @()
# Scanned before signing: a signature carries the certificate's subject on purpose.
Invoke-PrivacyScan $Out

# ---------------------------------------------------------------- sign + verify the setup
if ($Sign) {
  Invoke-Sign $Out
  Invoke-Verify $StagedExe
  Invoke-Verify $Out
}

$size = (Get-Item -LiteralPath $Out).Length
$sha = (Get-FileHash -LiteralPath $Out -Algorithm SHA256).Hash.ToLowerInvariant()
Write-Host ''
Write-Host ("Installer: {0}" -f $Out) -ForegroundColor Green
Write-Host ("Size:      {0:N2} MB ({1:N0} bytes)" -f ($size / 1MB), $size) -ForegroundColor Green
Write-Host ("SHA-256:   {0}" -f $sha) -ForegroundColor Green
Write-Host 'Privacy:   Sessionmux.exe, the staged files, the installer script and the setup exe (compressed and uncompressed) passed scripts\check-binary-privacy.ps1' -ForegroundColor Green
if ($Sign) {
  Write-Host "Signed:    Sessionmux.exe and the setup exe (verified with signtool verify /pa)" -ForegroundColor Green
} else {
  Write-Host 'Unsigned build: Windows SmartScreen will warn users until it is signed (-Sign, optional; see MAINTAINING.md).' -ForegroundColor Yellow
}
if ($size -gt 8MB) { Write-Host 'Warning: installer is larger than the 8 MB size target.' -ForegroundColor Yellow }
exit 0

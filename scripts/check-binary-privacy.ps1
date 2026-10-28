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
  Fails when a build artefact or image contains data that identifies the build machine or a person.

.DESCRIPTION
  Reads every given file as raw bytes and searches it twice, as single-byte text (ASCII/Latin-1) and
  as UTF-16LE text (both byte alignments), case-insensitively, for:
    - absolute paths                        any drive-letter path such as X:\dir\ (a few literals
                                            that are part of the source on purpose are allowed),
                                            \Users\ (either slash) and \Downloads\ with or without
                                            a drive letter,
                                            and the absolute path of this repository's checkout
    - toolchain folders                     \.cargo\  \.rustup\  (either slash)
    - debug-symbol references               .pdb
    - extended-length device paths          \\?\C:\...  \\?\UNC\...
    - the build machine's identity          $env:USERNAME, $env:COMPUTERNAME, $env:USERDOMAIN
                                            (on macOS and Linux: $env:USER and the host name)
                                            (generic values such as "user" or "runneradmin" are
                                            skipped; the path pattern above still catches them)
    - e-mail domains                        (set privately, see below)
    - time-zone names                       "... Standard Time"
    - the maintainer's personal name        the name part of the public handle joyal-droid
                                            (the handle itself is allowed)
    - private terms                         extra words that must not appear but must not be
                                            published in this script either, read at run time
                                            from $env:SESSIONMUX_PRIVACY_TERMS (separated by ';')
                                            and from scripts\private-terms.txt (one per line, '#'
                                            starts a comment; the file is git-ignored). A hit on
                                            a private term is reported with the term replaced by
                                            '*', and on GitHub Actions every term is also masked
                                            in the log
  Every PNG file is also checked for text metadata chunks (tEXt, iTXt, zTXt, eXIf).

  Every hit is printed with its file, encoding, byte offset and surrounding text. The script exits
  with 1 when anything is found, 2 on a usage error (missing file), 0 when every file is clean.
  A directory argument is expanded to all files below it.

.EXAMPLE
  powershell -File scripts\check-binary-privacy.ps1 target\release\Sessionmux.exe
.EXAMPLE
  powershell -File scripts\check-binary-privacy.ps1 dist\Sessionmux-Setup-1.0.0.exe (Get-ChildItem docs -Recurse -Filter *.png).FullName
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true, Position = 0, ValueFromRemainingArguments = $true)]
  [object[]]$Path
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

# ---------------------------------------------------------------- patterns
# Strings that are public on purpose. They are blanked out (same length, so offsets stay exact)
# before the patterns run.
# The drive-letter paths are literals in the app's own source: an example in a comment and the
# folders of the self-check's demo data.
$Allowed = @('joyal-droid', 'C:\a\b\', 'C:\Projects\')
# The handle can also be stored in pieces: a compiler may write a short string constant with a
# few eight-byte immediate stores, in any order, so "om/joyal" (from github.com/joyal-droid) and
# "-droid/s" end up apart. The name part right after "om/" counts as the handle.
$AllowedSplit = @('(?<=om/)joyal')

$Patterns = New-Object System.Collections.Generic.List[object]
function Add-Pattern([string]$name, [string]$regex) {
  # One regex is searched once; a second name for the same text is folded into the first.
  foreach ($existing in $Patterns) {
    if ($existing.Regex.ToString().ToLowerInvariant() -eq $regex.ToLowerInvariant()) { $existing.Name += " / $name"; return }
  }
  $Patterns.Add([pscustomobject]@{ Name = $name; Regex = New-Object System.Text.RegularExpressions.Regex($regex, 'IgnoreCase, CultureInvariant') })
}
Add-Pattern 'user-profile path' '[\\/]Users[\\/]'
# Backslashes only: "/downloads/" is an ordinary URL segment.
Add-Pattern 'downloads folder' '\\Downloads\\'
# A drive letter (upper case, as Windows writes it), a separator and a folder name of at least two
# characters followed by another separator: random binary data almost never looks like that.
Add-Pattern 'absolute path' '(?<![A-Za-z0-9])(?-i:[A-Z]):[\\/][A-Za-z0-9_ .$-]{2,}[\\/]'
Add-Pattern '.cargo folder' '[\\/]\.cargo[\\/]'
Add-Pattern '.rustup folder' '[\\/]\.rustup[\\/]'
Add-Pattern 'pdb reference' '\.pdb(?![A-Za-z0-9])'
Add-Pattern 'device path' '\\\\\?\\(?:[A-Z]:\\|UNC\\[A-Za-z0-9])'
Add-Pattern 'e-mail domain' 'gmail\.com'
Add-Pattern 'time-zone name' 'Standard Time'
Add-Pattern 'personal name' ([regex]::Escape(($Allowed[0] -split '-')[0]))

# The checkout's own absolute path (a CI runner's work folder, for example).
$RepoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..')).TrimEnd('\', '/')
if ($RepoRoot.Length -ge 4) { Add-Pattern 'repository path' ([regex]::Escape($RepoRoot)) }

# Private terms: never written into this public file.
$PrivateTerms = New-Object System.Collections.Generic.List[string]
if ($env:SESSIONMUX_PRIVACY_TERMS) {
  foreach ($t in ($env:SESSIONMUX_PRIVACY_TERMS -split ';')) { if ($t.Trim()) { $PrivateTerms.Add($t.Trim()) } }
}
$PrivateFile = Join-Path $PSScriptRoot 'private-terms.txt'
if (Test-Path -LiteralPath $PrivateFile) {
  foreach ($line in (Get-Content -LiteralPath $PrivateFile -Encoding UTF8)) {
    $t = ($line -replace '#.*$', '').Trim()
    if ($t) { $PrivateTerms.Add($t) }
  }
}
foreach ($t in $PrivateTerms) {
  # The secret as a whole is masked by GitHub; each single term must be masked as well.
  if ($env:GITHUB_ACTIONS -eq 'true') { Write-Host "::add-mask::$t" }
  Add-Pattern 'private term' ([regex]::Escape($t))
}

# Machine identity of whoever runs the scan. Generic account names would match ordinary words.
$Generic = @('user', 'users', 'admin', 'administrator', 'owner', 'guest', 'default', 'public', 'pc',
  'runner', 'runneradmin', 'builder', 'build', 'workgroup', 'localhost', 'system')
$Skipped = New-Object System.Collections.Generic.List[string]
# Each entry: the name shown when the value is skipped, the pattern's name and the value.
$Identity = @()
foreach ($var in @('USERNAME', 'COMPUTERNAME', 'USERDOMAIN')) {
  $Identity += , @($var, "`$env:$var", [Environment]::GetEnvironmentVariable($var))
}
# macOS and Linux keep the account name in $USER and have no COMPUTERNAME.
if ([System.IO.Path]::DirectorySeparatorChar -ne '\') {
  $Identity += , @('USER', '$env:USER', $env:USER)
  $Identity += , @('host name', 'host name', ([Environment]::MachineName -replace '\.local$', ''))
}
foreach ($entry in $Identity) {
  $value = $entry[2]
  if (-not $value) { continue }
  $value = $value.Trim()
  if ($value.Length -lt 3 -or ($Generic -contains $value.ToLowerInvariant())) {
    $Skipped.Add("$($entry[0]) (generic value)")
    continue
  }
  Add-Pattern $entry[1] ([regex]::Escape($value))
}

$PngTextChunks = @('tEXt', 'iTXt', 'zTXt', 'eXIf')

# ---------------------------------------------------------------- helpers
$Latin1 = [System.Text.Encoding]::GetEncoding(28591)

function Hide-Allowed([string]$text) {
  foreach ($a in $Allowed) {
    $text = [regex]::Replace($text, [regex]::Escape($a), { param($m) '#' * $m.Length }, 'IgnoreCase')
  }
  foreach ($a in $AllowedSplit) {
    $text = [regex]::Replace($text, $a, { param($m) '#' * $m.Length }, 'IgnoreCase')
  }
  return $text
}

# Same-length copy of the text with every private term replaced by '*', used only for printing.
function Hide-Private([string]$text) {
  foreach ($t in $PrivateTerms) {
    $text = [regex]::Replace($text, [regex]::Escape($t), { param($m) '*' * $m.Length }, 'IgnoreCase')
  }
  return $text
}

function Format-Context([string]$text, [int]$index, [int]$length) {
  $start = [Math]::Max(0, $index - 40)
  $end = [Math]::Min($text.Length, $index + $length + 40)
  $sb = New-Object System.Text.StringBuilder
  for ($i = $start; $i -lt $end; $i++) {
    $c = $text[$i]
    if ([int]$c -ge 0x20 -and [int]$c -lt 0x7F) { [void]$sb.Append($c) } else { [void]$sb.Append('.') }
  }
  return $sb.ToString()
}

function Get-Views([byte[]]$bytes) {
  # Each view maps character index i to byte offset (Base + i * Step).
  $views = @([pscustomobject]@{ Encoding = 'ascii'; Base = 0; Step = 1; Text = $Latin1.GetString($bytes) })
  foreach ($align in 0, 1) {
    $n = [int][Math]::Floor(($bytes.Length - $align) / 2)
    if ($n -le 0) { continue }
    $chars = New-Object char[] $n
    [Buffer]::BlockCopy($bytes, $align, $chars, 0, $n * 2)
    $views += [pscustomobject]@{ Encoding = 'utf-16le'; Base = $align; Step = 2; Text = (New-Object string (, $chars)) }
  }
  return $views
}

function Get-PngTextChunks([byte[]]$bytes) {
  $found = @()
  $sig = [byte[]](0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A)
  if ($bytes.Length -lt 8) { return $found }
  for ($i = 0; $i -lt 8; $i++) { if ($bytes[$i] -ne $sig[$i]) { return $found } }
  $pos = 8
  while ($pos + 8 -le $bytes.Length) {
    $len = ([uint32]$bytes[$pos] -shl 24) -bor ([uint32]$bytes[$pos + 1] -shl 16) -bor ([uint32]$bytes[$pos + 2] -shl 8) -bor [uint32]$bytes[$pos + 3]
    $type = $Latin1.GetString($bytes, $pos + 4, 4)
    if ($PngTextChunks -ccontains $type) {
      $dataLen = [int][Math]::Min([double]$len, [double]($bytes.Length - $pos - 8))
      $preview = Format-Context (Hide-Private ($Latin1.GetString($bytes, $pos + 8, [Math]::Max(0, [Math]::Min($dataLen, 80))))) 0 0
      $found += [pscustomobject]@{ Offset = $pos; Type = $type; Preview = $preview }
    }
    if ($type -ceq 'IEND') { break }
    $pos = $pos + 12 + [int64]$len
  }
  return $found
}

# ---------------------------------------------------------------- expand arguments
$Files = New-Object System.Collections.Generic.List[string]
# Arguments may arrive as nested arrays, e.g. (Get-ChildItem docs -Filter *.png).FullName.
$Flat = @($Path | ForEach-Object { $_ } | ForEach-Object { [string]$_ })
foreach ($p in $Flat) {
  if (-not $p) { continue }
  $items = @(Get-Item -Path $p -Force -ErrorAction SilentlyContinue)
  if ($items.Count -eq 0) {
    Write-Host "check-binary-privacy: not found: $p" -ForegroundColor Red
    exit 2
  }
  foreach ($item in $items) {
    if ($item.PSIsContainer) {
      Get-ChildItem -LiteralPath $item.FullName -Recurse -File -Force | ForEach-Object { $Files.Add($_.FullName) }
    } else {
      $Files.Add($item.FullName)
    }
  }
}
if ($Files.Count -eq 0) {
  Write-Host 'check-binary-privacy: no files to scan' -ForegroundColor Red
  exit 2
}
if ($Skipped.Count -gt 0) { Write-Host "check-binary-privacy: not searched: $($Skipped -join ', ')" }
Write-Host "check-binary-privacy: $($Patterns.Count) pattern(s), $($PrivateTerms.Count) private term(s)"

# ---------------------------------------------------------------- scan
$total = 0
foreach ($file in $Files) {
  $bytes = [System.IO.File]::ReadAllBytes($file)
  $hits = New-Object System.Collections.Generic.List[string]
  foreach ($view in (Get-Views $bytes)) {
    $text = Hide-Allowed $view.Text
    $shown = $null
    foreach ($pat in $Patterns) {
      foreach ($m in $pat.Regex.Matches($text)) {
        if ($null -eq $shown) { $shown = Hide-Private $text }
        $offset = $view.Base + [int64]$m.Index * $view.Step
        $hits.Add(('  {0,-18} {1,-8} offset 0x{2:X8}  "{3}"' -f $pat.Name, $view.Encoding, $offset, (Format-Context $shown $m.Index $m.Length)))
      }
    }
  }
  if ([System.IO.Path]::GetExtension($file) -ieq '.png') {
    foreach ($chunk in (Get-PngTextChunks $bytes)) {
      $hits.Add(('  {0,-18} {1,-8} offset 0x{2:X8}  "{3}"' -f 'png text chunk', $chunk.Type, $chunk.Offset, $chunk.Preview))
    }
  }
  if ($hits.Count -eq 0) {
    Write-Host ("clean  {0} ({1:N0} bytes)" -f $file, $bytes.Length)
  } else {
    Write-Host ("FOUND  {0}: {1} hit(s)" -f $file, $hits.Count) -ForegroundColor Red
    foreach ($h in $hits) { Write-Host $h }
    $total += $hits.Count
  }
}

if ($total -gt 0) {
  Write-Host "check-binary-privacy: $total hit(s) in $($Files.Count) file(s)" -ForegroundColor Red
  exit 1
}
Write-Host "check-binary-privacy: $($Files.Count) file(s) clean" -ForegroundColor Green
exit 0

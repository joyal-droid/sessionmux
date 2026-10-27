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
  Copies the README screenshots from smoke\ to docs\screenshots\ as compressed PNGs.

.DESCRIPTION
  The smoke run writes uncompressed PNGs. This re-encodes each one with System.Drawing
  (deflate-compressed PNG) and fails when a result is larger than -MaxKB.
  Run the demo fixture first, from the repository root:

    $env:SESSIONMUX_SMOKE_FIXTURE = '1'; .\target\release\Sessionmux.exe --smoke
    .\scripts\compress-png.ps1
#>
[CmdletBinding()]
param(
    [string]$Source = (Join-Path $PSScriptRoot '..\smoke'),
    [string]$Destination = (Join-Path $PSScriptRoot '..\docs\screenshots'),
    [string[]]$Names = @('bubble', 'bubble-tray', 'panel-accounts', 'panel-sessions', 'panel-approvals', 'panel-settings', 'panel-about', 'signin-notice'),
    [int]$MaxKB = 150
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

# Captures that are cut into horizontal bands (fractions of the height) and stacked. The About
# capture keeps the header and the About card and drops the folder list above it, whose paths
# would show where the capture was made.
$BandTable = @{
    'panel-about' = @(@(0.0, 0.099), @(0.49, 1.0))
}

New-Item -ItemType Directory -Force -Path $Destination | Out-Null
$failed = $false
foreach ($name in $Names) {
    $in = Join-Path $Source "$name.png"
    if (-not (Test-Path $in)) { throw "missing $in (run the smoke test with SESSIONMUX_SMOKE_FIXTURE=1 first)" }
    $out = Join-Path $Destination "$name.png"
    # Load from a memory copy so the source file is not kept open.
    $bytes = [System.IO.File]::ReadAllBytes((Resolve-Path $in))
    $ms = New-Object System.IO.MemoryStream(, $bytes)
    $img = [System.Drawing.Image]::FromStream($ms)
    try {
        $bands = @(, @(0.0, 1.0))
        if ($BandTable.ContainsKey($name)) { $bands = $BandTable[$name] }
        $rows = New-Object System.Collections.Generic.List[int[]]
        $height = 0
        foreach ($b in $bands) {
            $y0 = [int][math]::Round($b[0] * $img.Height); $y1 = [int][math]::Round($b[1] * $img.Height)
            $rows.Add([int[]]@($y0, ($y1 - $y0)))
            $height += $y1 - $y0
        }
        $bmp = New-Object System.Drawing.Bitmap($img.Width, [int]$height, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
        try {
            $g = [System.Drawing.Graphics]::FromImage($bmp)
            $y = 0
            foreach ($r in $rows) {
                $src = New-Object System.Drawing.Rectangle(0, $r[0], $img.Width, $r[1])
                $dst = New-Object System.Drawing.Rectangle(0, $y, $img.Width, $r[1])
                $g.DrawImage($img, $dst, $src, [System.Drawing.GraphicsUnit]::Pixel)
                $y += $r[1]
            }
            $g.Dispose()
            $bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
        } finally { $bmp.Dispose() }
    } finally { $img.Dispose(); $ms.Dispose() }
    $kb = [math]::Round((Get-Item $out).Length / 1KB, 1)
    $note = ''
    if ($kb -gt $MaxKB) { $failed = $true; $note = "  (over $MaxKB KB)" }
    Write-Host ("{0,-20} {1,8} KB{2}" -f "$name.png", $kb, $note)
}
if ($failed) { exit 1 }

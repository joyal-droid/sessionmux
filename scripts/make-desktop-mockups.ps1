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
  Draws docs\screenshots\desktop-tray.png and docs\screenshots\desktop-panel.png, two 1600x1000
  pictures of Sessionmux on a clean desktop.

.DESCRIPTION
  The desktop is the soft gradient wallpaper alone: no taskbar, no icons, no other windows. The
  demo-fixture captures from docs\screenshots are placed on it at their native size (never
  scaled), so the app stays pixel-sharp, each with a soft shadow and a 1 px light inner border:
    desktop-tray.png   the mini tray (bubble-tray.png) snapped to the right edge, with one chip
    desktop-panel.png  the same scene with the Accounts panel (panel-accounts.png) open beside
                       it, with one chip next to "Add account"
  The work area ends 48 px above the bottom edge, where the taskbar would be. The drawing helpers
  are in mockup-common.ps1. Regenerate after the captures change, from the repository root:

    .\scripts\compress-png.ps1
    .\scripts\make-desktop-mockups.ps1
#>
[CmdletBinding()]
param(
    [string]$Screenshots = (Join-Path $PSScriptRoot '..\docs\screenshots'),
    [string]$OutDir = (Join-Path $PSScriptRoot '..\docs\screenshots'),
    [int]$MaxKB = 500
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
. (Join-Path $PSScriptRoot 'mockup-common.ps1')

$W = 1600; $H = 1000
$WorkBottom = $H - 48
$Margin = 12          # distance of the mini tray from the screen edge and from the work area's end
$PanelGap = 12        # gap between the mini tray and the panel
$ChipGap = 24         # distance of a chip from the UI it describes
$AddAccountRow = 0.946   # vertical centre of the "Add account" row, as a fraction of the panel height

# The blobs sit away from the lower right, so the mini tray and the panel stand on the light base.
$Blobs = @(
    @{ X = 0.10; Y = 0.12; R = 0.50; Color = '#7C3AED'; Alpha = 0.58 },
    @{ X = 0.55; Y = 1.00; R = 0.50; Color = '#2563EB'; Alpha = 0.50 },
    @{ X = 0.92; Y = 0.10; R = 0.45; Color = '#C7D2FE'; Alpha = 0.95 },
    @{ X = 0.95; Y = 0.80; R = 0.35; Color = '#EEF2FF'; Alpha = 0.90 }
)

$tray = Read-Image (Join-Path $Screenshots 'bubble-tray.png')
$panel = Read-Image (Join-Path $Screenshots 'panel-accounts.png')
$pill = Get-OpaqueBounds $tray
$radius = Get-CaptureRadius $panel

# Mini tray: the pill sits $Margin px from the right edge and from the end of the work area.
$trayX = $W - $Margin - $pill.Right
$trayY = $WorkBottom - $Margin - $pill.Bottom
$pillLeft = $trayX + $pill.X
$pillTop = $trayY + $pill.Y
$pillBottom = $trayY + $pill.Bottom
# Panel: left of the pill with $PanelGap, centred on it and clamped into the work area, as the
# app places it; here the clamp keeps its bottom level with the pill.
$panelX = $pillLeft - $PanelGap - $panel.Width
$panelY = [int](($pillTop + $pillBottom) / 2 - $panel.Height / 2)
if ($panelY + $panel.Height -gt $pillBottom) { $panelY = $pillBottom - $panel.Height }
if ($panelY -lt $Margin) { throw 'the panel capture does not fit in the work area' }

$wallpaper = New-Wallpaper $W $H $Blobs

function Save-Mockup([string]$name, [bool]$withPanel) {
    $bmp = New-Object System.Drawing.Bitmap($wallpaper)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    try {
        Set-Quality $g
        if ($withPanel) {
            Draw-Window $g $panel $panelX $panelY $radius
            [void](Draw-Chip $g 'Google and Apple sign-in' ($panelX - $ChipGap) ($panelY + $AddAccountRow * $panel.Height) -AlignRight)
        } else {
            [void](Draw-Chip $g 'Switch without logging out' ($pillLeft - $ChipGap) (($pillTop + $pillBottom) / 2) -AlignRight)
        }
        Draw-Tray $g $tray $pill $trayX $trayY
        $out = Join-Path $OutDir "$name.png"
        $kb = Save-CleanPng $bmp $out
    } finally {
        $g.Dispose(); $bmp.Dispose()
    }
    Write-Host ("{0}.png {1}x{2} {3} KB" -f $name, $W, $H, $kb)
    if ($kb -gt $MaxKB) { throw "$name.png is over $MaxKB KB" }
}

try {
    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
    Save-Mockup 'desktop-tray' $false
    Save-Mockup 'desktop-panel' $true
    Write-Host ("mini tray at {0},{1} ({2}x{3}); panel at {4},{5} ({6}x{7})" -f $trayX, $trayY, $tray.Width, $tray.Height, $panelX, $panelY, $panel.Width, $panel.Height)
} finally {
    $wallpaper.Dispose(); $tray.Dispose(); $panel.Dispose()
}

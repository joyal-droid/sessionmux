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
  Draws docs\screenshots\hero.png, the 1600x900 image at the top of the README.

.DESCRIPTION
  The demo-fixture Accounts panel (docs\screenshots\panel-accounts.png) centred on the soft
  gradient wallpaper, the mini tray (bubble-tray.png) in the top-right corner, and two callout
  chips beside the panel. Both captures are drawn at their native size with a soft shadow and a
  1 px light inner border; the chips sit outside the panel, next to the rows they describe. The
  drawing helpers are in mockup-common.ps1. Regenerate after the screenshots change, from the
  repository root:

    .\scripts\compress-png.ps1
    .\scripts\make-hero.ps1
#>
[CmdletBinding()]
param(
    [string]$Screenshots = (Join-Path $PSScriptRoot '..\docs\screenshots'),
    # Accepted so existing command lines keep working; the hero shows no mark.
    [string]$Icon = (Join-Path $PSScriptRoot '..\assets\icon.ico'),
    [string]$Out = (Join-Path $PSScriptRoot '..\docs\screenshots\hero.png'),
    [int]$MaxKB = 450
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
. (Join-Path $PSScriptRoot 'mockup-common.ps1')

$W = 1600; $H = 900
$Inset = 48            # distance of the mini tray's pill from the top and right edges
$ChipGap = 28          # distance of a chip from the panel
# Vertical centres of the first two account rows in the panel capture, as fractions of its height.
$WorkRow = 0.172; $PersonalRow = 0.300

$tray = Read-Image (Join-Path $Screenshots 'bubble-tray.png')
$panel = Read-Image (Join-Path $Screenshots 'panel-accounts.png')
$pill = Get-OpaqueBounds $tray
$radius = Get-CaptureRadius $panel

$px = [int](($W - $panel.Width) / 2)
$py = [int](($H - $panel.Height) / 2)
$tx = $W - $Inset - $pill.Right
$ty = $Inset - $pill.Y

$bmp = New-Wallpaper $W $H
$g = [System.Drawing.Graphics]::FromImage($bmp)
try {
    Set-Quality $g
    Draw-Window $g $panel $px $py $radius
    Draw-Tray $g $tray $pill $tx $ty
    [void](Draw-Chip $g 'Switch without logging out' ($px - $ChipGap) ($py + $PersonalRow * $panel.Height) -AlignRight)
    [void](Draw-Chip $g 'Move Claude Code sessions' ($px + $panel.Width + $ChipGap) ($py + $WorkRow * $panel.Height))
    $kb = Save-CleanPng $bmp $Out
} finally {
    $g.Dispose(); $bmp.Dispose(); $tray.Dispose(); $panel.Dispose()
}

Write-Host ("hero.png {0}x{1} {2} KB" -f $W, $H, $kb)
if ($kb -gt $MaxKB) { Write-Error "hero.png is over $MaxKB KB"; exit 1 }

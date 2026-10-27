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
  Draws docs\social-preview.png, the 1280x640 image used as the repository's social preview and
  as the Open Graph image of the website.

.DESCRIPTION
  Left half on white: the Sessionmux mark (drawn here as vectors, 96 px), the "Sessionmux"
  wordmark, the sub-line "Multiplex Claude Desktop" and three one-line features in #444. Right
  half: the soft gradient wallpaper with the demo-fixture Accounts panel capture
  (docs\screenshots\panel-accounts.png) at native size, soft shadow and 1 px light inner border,
  running off the bottom edge. The drawing helpers are in mockup-common.ps1. Regenerate after
  the screenshots change, from the repository root:

    .\scripts\make-social-preview.ps1
#>
[CmdletBinding()]
param(
    [string]$Screenshots = (Join-Path $PSScriptRoot '..\docs\screenshots'),
    [string]$Out = (Join-Path $PSScriptRoot '..\docs\social-preview.png'),
    [int]$MaxKB = 300
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
. (Join-Path $PSScriptRoot 'mockup-common.ps1')

$W = 1280; $H = 640
$Half = 640
$Ink = '#0A0A0A'

# The mark on a 24-unit grid, the same geometry as the Sessionmux mark everywhere else: a rounded
# tile (x and y 3.5 to 20.5, corner radius 3.7) split by a slanted gap into two panes, the gap
# running from x 14.1-15.7 at the top to x 8.9-10.5 at the bottom. Both panes are filled by one
# #7C3AED -> #2563EB gradient laid over the tile bounds, from top left to bottom right.
function Draw-Mark($g, [float]$x, [float]$y, [float]$size) {
    $state = $g.Save()
    $g.TranslateTransform($x, $y)
    $g.ScaleTransform($size / 24, $size / 24)

    $left = New-Object System.Drawing.Drawing2D.GraphicsPath
    $left.AddLine([float]7.2, [float]3.5, [float]14.1, [float]3.5)
    $left.AddLine([float]14.1, [float]3.5, [float]8.9, [float]20.5)
    $left.AddLine([float]8.9, [float]20.5, [float]7.2, [float]20.5)
    $left.AddArc([float]3.5, [float]13.1, [float]7.4, [float]7.4, 90, 90)
    $left.AddArc([float]3.5, [float]3.5, [float]7.4, [float]7.4, 180, 90)
    $left.CloseFigure()

    $right = New-Object System.Drawing.Drawing2D.GraphicsPath
    $right.AddLine([float]15.7, [float]3.5, [float]16.8, [float]3.5)
    $right.AddArc([float]13.1, [float]3.5, [float]7.4, [float]7.4, 270, 90)
    $right.AddArc([float]13.1, [float]13.1, [float]7.4, [float]7.4, 0, 90)
    $right.AddLine([float]16.8, [float]20.5, [float]10.5, [float]20.5)
    $right.CloseFigure()

    $box = [System.Drawing.RectangleF]::new(3.4, 3.4, 17.2, 17.2)
    $grad = New-Object System.Drawing.Drawing2D.LinearGradientBrush($box, (C '#7C3AED'), (C '#2563EB'), [float]45, $true)
    $grad.WrapMode = [System.Drawing.Drawing2D.WrapMode]::TileFlipXY
    $g.FillPath($grad, $left)
    $g.FillPath($grad, $right)

    $g.Restore($state)
    foreach ($o in @($left, $right, $grad)) { $o.Dispose() }
}

# Draws text with extra tracking (negative = tighter), one character at a time.
function Draw-Tracked($g, [string]$text, $font, $brush, [float]$x, [float]$y, [float]$tracking) {
    $fmt = [System.Drawing.StringFormat]::GenericTypographic
    $cx = $x
    foreach ($ch in $text.ToCharArray()) {
        $s = [string]$ch
        $g.DrawString($s, $font, $brush, $cx, $y, $fmt)
        $cx += $g.MeasureString($s, $font, 1000, $fmt).Width + $tracking
    }
    return $cx
}

$panel = Read-Image (Join-Path $Screenshots 'panel-accounts.png')
$radius = Get-CaptureRadius $panel

# The right half: wallpaper with the panel, cropped by the bottom edge.
$Blobs = @(
    @{ X = 0.05; Y = 0.05; R = 0.55; Color = '#7C3AED'; Alpha = 0.60 },
    @{ X = 1.00; Y = 1.00; R = 0.60; Color = '#2563EB'; Alpha = 0.55 },
    @{ X = 0.95; Y = 0.05; R = 0.45; Color = '#C7D2FE'; Alpha = 0.95 },
    @{ X = 0.05; Y = 0.95; R = 0.45; Color = '#EEF2FF'; Alpha = 0.95 }
)
$right = New-Wallpaper $Half $H $Blobs

$bmp = New-Object System.Drawing.Bitmap($W, $H, [System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
$g = [System.Drawing.Graphics]::FromImage($bmp)
try {
    Set-Quality $g
    $g.Clear([System.Drawing.Color]::White)

    $g.DrawImageUnscaled($right, $Half, 0)
    $px = $Half + [int](($Half - $panel.Width) / 2)
    Draw-Window $g $panel $px 96 $radius

    $left = 80
    $unit = [System.Drawing.GraphicsUnit]::Pixel
    $fName = New-Object System.Drawing.Font('Segoe UI', 88, [System.Drawing.FontStyle]::Bold, $unit)
    $fSub = New-Object System.Drawing.Font('Segoe UI Semibold', 32, [System.Drawing.FontStyle]::Regular, $unit)
    $fFeat = New-Object System.Drawing.Font('Segoe UI', 24, [System.Drawing.FontStyle]::Regular, $unit)
    $bInk = New-Object System.Drawing.SolidBrush((C $Ink))
    $bFeat = New-Object System.Drawing.SolidBrush((C '#444444'))
    $fmt = [System.Drawing.StringFormat]::GenericTypographic

    Draw-Mark $g ($left - 14) 104 96
    [void](Draw-Tracked $g 'Sessionmux' $fName $bInk ($left - 5) 214 (-0.035 * 88))
    $g.DrawString('Multiplex Claude Desktop', $fSub, $bInk, [float]($left - 1), [float]334, $fmt)
    $features = @(
        'Switch accounts without logging out',
        'Move Claude Code sessions between accounts',
        'Sign in to extra accounts with Google or Apple'
    )
    $fy = 414
    foreach ($f in $features) {
        $g.DrawString($f, $fFeat, $bFeat, [float]$left, [float]$fy, $fmt)
        $fy += 42
    }

    foreach ($o in @($fName, $fSub, $fFeat, $bInk, $bFeat)) { $o.Dispose() }
    $kb = Save-CleanPng $bmp $Out
} finally {
    $g.Dispose(); $bmp.Dispose(); $right.Dispose(); $panel.Dispose()
}

Write-Host ("social-preview.png {0}x{1} {2} KB" -f $W, $H, $kb)
if ($kb -gt $MaxKB) { Write-Error "social-preview.png is over $MaxKB KB"; exit 1 }

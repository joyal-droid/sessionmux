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
  Draws docs\download-button.png (Windows) or docs\download-button-macos.png (macOS), the
  download buttons shown near the top of the README.

.DESCRIPTION
  A 360x72 dark card (#0a0a0a, 1 px #333333 border, 12 px radius) with a 4 px white bar along
  its left edge, a 48x48 #1f1f1f tile holding a white download glyph (drawn here as vectors), a
  small letter-spaced monospace caption in #9a9a9a naming the installer, its platform and size,
  and the label "Download Latest" in bold white. The corners outside the card are transparent so
  it sits on light and dark pages alike. It is rendered at -Scale times its logical size (720x144
  by default) and the README displays it at width 360. Uses System.Drawing only. Regenerate after
  the product name or the installer size changes, from the repository root:

    .\scripts\make-download-button.ps1 -SizeMb 0.55

  -Platform macos draws the macOS button in the same style: the caption names the disk image,
  the minimum macOS version and its size, the label reads "Download for macOS", and -Preview adds
  a small outlined "PREVIEW" tag after the label. It uses no operating-system logo, only the same
  download glyph. -SizeMb is required for it:

    .\scripts\make-download-button.ps1 -Platform macos -SizeMb 5.2 -Preview
#>
[CmdletBinding()]
param(
    [string]$Name = 'Sessionmux',
    [string]$Repo = 'joyal-droid/sessionmux',
    [double]$SizeMb = 0.55,
    [string]$Out,
    [int]$Scale = 2,
    [int]$MaxKB = 60,
    [ValidateSet('windows', 'macos')]
    [string]$Platform = 'windows',
    # The button's text; the default depends on -Platform.
    [string]$Label,
    # macOS only: adds a "PREVIEW" tag after the label.
    [switch]$Preview
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$mac = $Platform -eq 'macos'
if ($mac -and -not $PSBoundParameters.ContainsKey('SizeMb')) { throw '-Platform macos needs -SizeMb (the size of the disk image)' }
if ($Preview -and -not $mac) { throw '-Preview is for -Platform macos only' }
if (-not $Out) {
    $Out = if ($mac) { Join-Path $PSScriptRoot '..\docs\download-button-macos.png' } else { Join-Path $PSScriptRoot '..\docs\download-button.png' }
}

# Logical size; the bitmap is $Scale times larger.
$LW = 360; $LH = 72
$card = [System.Drawing.ColorTranslator]::FromHtml('#0A0A0A')
$border = [System.Drawing.ColorTranslator]::FromHtml('#333333')
$tile = [System.Drawing.ColorTranslator]::FromHtml('#1F1F1F')
$caption = [System.Drawing.ColorTranslator]::FromHtml('#9A9A9A')
$white = [System.Drawing.ColorTranslator]::FromHtml('#FFFFFF')

function New-RoundRect([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = 2 * $r
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

# The download glyph on a 24-unit grid: a tray (open at the top, rounded bottom corners) and an
# arrow pointing down into it, stroke 2 with round caps and joins.
function Draw-DownloadGlyph($g, [float]$x, [float]$y, [float]$size, $color) {
    $k = $size / 24
    $state = $g.Save()
    $g.TranslateTransform($x, $y)
    $g.ScaleTransform($k, $k)
    $pen = New-Object System.Drawing.Pen($color, [float]2)
    $pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
    $tray = New-Object System.Drawing.Drawing2D.GraphicsPath
    $tray.AddLine([float]3, [float]15, [float]3, [float]19)
    $tray.AddArc([float]3, [float]17, [float]4, [float]4, 180, -90)
    $tray.AddLine([float]5, [float]21, [float]19, [float]21)
    $tray.AddArc([float]17, [float]17, [float]4, [float]4, 90, -90)
    $tray.AddLine([float]21, [float]19, [float]21, [float]15)
    $g.DrawPath($pen, $tray)
    $g.DrawLine($pen, [float]12, [float]3, [float]12, [float]15)
    $arrow = New-Object System.Drawing.Drawing2D.GraphicsPath
    $arrow.AddLines([System.Drawing.PointF[]]@(
        [System.Drawing.PointF]::new(7, 10), [System.Drawing.PointF]::new(12, 15), [System.Drawing.PointF]::new(17, 10)))
    $g.DrawPath($pen, $arrow)
    $g.Restore($state)
    $tray.Dispose(); $arrow.Dispose(); $pen.Dispose()
}

# Draws text with extra tracking (negative = tighter), one character at a time.
function Draw-Tracked($g, [string]$text, $font, $brush, [float]$x, [float]$y, [float]$tracking) {
    $fmt = [System.Drawing.StringFormat]::GenericTypographic.Clone()
    $fmt.FormatFlags = $fmt.FormatFlags -bor [System.Drawing.StringFormatFlags]::MeasureTrailingSpaces
    $cx = $x
    foreach ($ch in $text.ToCharArray()) {
        $s = [string]$ch
        $g.DrawString($s, $font, $brush, $cx, $y, $fmt)
        $cx += $g.MeasureString($s, $font, 1000, $fmt).Width + $tracking
    }
    return $cx
}

$dot = ' ' + [char]0x00B7 + ' '   # middle dot, kept out of the source so the script stays ASCII
$size = $SizeMb.ToString('0.0#', [System.Globalization.CultureInfo]::InvariantCulture)
if ($mac) {
    $captionText = ($Name + '-macos.dmg' + $dot + 'macos 13+' + $dot + $size + ' mb').ToLowerInvariant()
    $labelText = if ($Label) { $Label } else { 'Download for macOS' }
} else {
    $captionText = ($Name + '-setup.exe' + $dot + 'windows' + $dot + $size + ' mb').ToLowerInvariant()
    $labelText = if ($Label) { $Label } else { 'Download Latest' }
}

$W = $LW * $Scale; $H = $LH * $Scale
$bmp = New-Object System.Drawing.Bitmap($W, $H, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$outPath = [System.IO.Path]::GetFullPath($Out)
try {
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    # Greyscale anti-aliasing, not ClearType: the colour fringes of ClearType look wrong once the
    # image is scaled or shown on another background.
    $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
    $g.Clear([System.Drawing.Color]::Transparent)
    $g.ScaleTransform($Scale, $Scale)

    $px = [System.Drawing.GraphicsUnit]::Pixel
    $fCaption = New-Object System.Drawing.Font('Consolas', 11, [System.Drawing.FontStyle]::Regular, $px)
    $fLabel = New-Object System.Drawing.Font('Segoe UI', 20, [System.Drawing.FontStyle]::Bold, $px)
    $bCard = New-Object System.Drawing.SolidBrush($card)
    $bTile = New-Object System.Drawing.SolidBrush($tile)
    $bCaption = New-Object System.Drawing.SolidBrush($caption)
    $bWhite = New-Object System.Drawing.SolidBrush($white)
    $penBorder = New-Object System.Drawing.Pen($border, 1)

    # Card, then the bar: the card's own outline filled white inside a 4 px strip, so the bar
    # follows the rounded corners with smooth edges. The border goes on top.
    $body = New-RoundRect 0.5 0.5 ($LW - 1) ($LH - 1) 12
    $g.FillPath($bCard, $body)
    $g.SetClip([System.Drawing.RectangleF]::new(0, 0, 4, $LH))
    $g.FillPath($bWhite, $body)
    $g.ResetClip()
    $g.DrawPath($penBorder, $body)
    $body.Dispose()

    # Icon tile and glyph.
    $tx = 16; $ty = ($LH - 48) / 2
    $tilePath = New-RoundRect $tx $ty 48 48 10
    $g.FillPath($bTile, $tilePath)
    $tilePath.Dispose()
    Draw-DownloadGlyph $g ($tx + 12) ($ty + 12) 24 $white

    # Caption and label, as one block centred on the card's height.
    $left = $tx + 48 + 14
    [void](Draw-Tracked $g $captionText $fCaption $bCaption $left 16 (0.06 * 11))
    $labelEnd = Draw-Tracked $g $labelText $fLabel $bWhite ($left - 1) 32 (-0.01 * 20)

    # The preview tag: letter-spaced caption type in a 1 px outlined pill, centred on the label's
    # line, in the caption's grey so the label stays the strongest text on the card.
    if ($Preview) {
        $fTag = New-Object System.Drawing.Font('Consolas', 10, [System.Drawing.FontStyle]::Bold, $px)
        $fmt = [System.Drawing.StringFormat]::GenericTypographic
        $tagText = 'PREVIEW'
        $tagW = 0
        foreach ($ch in $tagText.ToCharArray()) { $tagW += $g.MeasureString([string]$ch, $fTag, 1000, $fmt).Width + (0.08 * 10) }
        $tagW = [math]::Ceiling($tagW - (0.08 * 10) + 12)
        $tagH = 18
        $tagX = [math]::Round($labelEnd + 8)
        $tagY = 37
        if ($tagX + $tagW -gt $LW - 12) { throw "the label is too long for the PREVIEW tag ($labelText)" }
        $penTag = New-Object System.Drawing.Pen($caption, 1)
        $tagPath = New-RoundRect ($tagX + 0.5) ($tagY + 0.5) ($tagW - 1) ($tagH - 1) 9
        $g.DrawPath($penTag, $tagPath)
        [void](Draw-Tracked $g $tagText $fTag $bCaption ($tagX + 6) ($tagY + 3.5) (0.08 * 10))
        $tagPath.Dispose(); $penTag.Dispose(); $fTag.Dispose()
    }

    foreach ($o in @($fCaption, $fLabel, $bCard, $bTile, $bCaption, $bWhite, $penBorder)) { $o.Dispose() }

    # Encode in memory and write the bytes, so the file carries nothing but the image.
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    [System.IO.File]::WriteAllBytes($outPath, $ms.ToArray())
    $ms.Dispose()
} finally {
    $g.Dispose(); $bmp.Dispose()
}

# Re-encode through the same step as the screenshots.
$leaf = [System.IO.Path]::GetFileNameWithoutExtension($outPath)
$dir = [System.IO.Path]::GetDirectoryName($outPath)
& (Join-Path $PSScriptRoot 'compress-png.ps1') -Source $dir -Destination $dir -Names @($leaf) -MaxKB $MaxKB
if ($LASTEXITCODE) { exit $LASTEXITCODE }

$kb = [math]::Round((Get-Item $outPath).Length / 1KB, 1)
$target = if ($mac) { 'the macOS download of the release' } else { "https://github.com/$Repo/releases/latest" }
Write-Host ("{0} {1}x{2} {3} KB, links to {4}" -f "$leaf.png", $W, $H, $kb, $target)
